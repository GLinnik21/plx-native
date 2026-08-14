# plex/ — the Plex data layer (typed PMS + plex.tv client)

This is the **typed Plex client**: account/login (`account.rs`, `session.rs`), the HTTP client
(`client.rs` — one server) and the registry that holds them (`servers.rs` — WHICH servers exist and
which is current, with `probe.rs` deciding which of a server's addresses is worth dialling),
library/hubs/metadata reads (`library.rs`/`hubs.rs`/`models.rs`), and the whole
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

## More than one server: the registry (`servers.rs`)

**A friend's shared server is not a second address, it is a second AUTHORITY** — its own
`machineIdentifier`, its own per-(user, server) `accessToken`, its own `ratingKey` space and its own
watch state. Measured live against a real share (`docs/shared-servers.md` §2): our own server's
token gets a **401** from it, and its section key `1` is a different library from our section key
`1`. So this layer is keyed on servers, not on one host and port.

`client()` and `client_opt()` still mean what they always did, they just mean **the CURRENT
server** now — which is why nothing outside `plex/` changed when the `OnceLock<Client>` singleton
became a table. `client_for(id)` is the multi-server addition; `register(machine_id, host, port,
token)` puts a server in the table; `install(host, port, token)` is the SESSION path (boot, QR
login, profile switch) and always retargets. Slots are keyed on `machineIdentifier` because that is
the only identity that survives a server changing address — and a `register` that has *learned* an
id **adopts** an address-only slot instead of adding a second one for the same machine.

Three design choices carry the weight, and each is a prevented bug rather than a preference:

- **An atomic-pointer table, not an `RwLock`.** `client()` is a HOT path: `posters::poster_key`
  calls it **three times per key, for every visible art tile, every frame** (~25–40 tiles × 60 fps).
  A read is one relaxed load, one acquire load, a deref — no lock, no refcount, no allocation. An
  `RwLock` would add an atomic RMW pair per call plus a fairness stall every time a login writes,
  and an `Arc` would change every call site's type to buy a refcount bump per tile per frame.
- **Each slot's `Client` is LEAKED, deliberately.** That is what makes handing out `&'static
  Client` sound without an `Arc`: the reference a worker took at frame N must still be valid when a
  re-point lands at frame N+1 with that worker mid-request. Re-pointing publishes a NEW leaked
  `Client` over the pointer, so the worst case for the old reference is **one request sent to where
  that server used to be** — never a dangling pointer, which is the failure that has no debugger on
  this device. The leak is bounded: a handful of small structs, written on login / profile switch /
  server switch, never per frame.
- **Token generations come from a process-global sequence, so no two clients ever share one.**
  `token_gen` was a single process-wide counter, which cannot express "server B's token changed".
  Its only reader is `posters::poster_key`'s memo and that memo compares **one number** — so two
  servers whose generations happened to agree would mean that the moment `client()` started
  answering with B, the memo said nothing had changed and served B its cards from **A's memoised,
  token-bearing paths**. Uniqueness makes "did this number move" also answer "is this even the same
  server".

## Granted, pinned, reachable — three states, and never the same question

The whole shared-source feature rests on keeping these apart:

- **granted** — plex.tv's answer. `/api/v2/resources` says this account may use this server and
  hands over the `accessToken` that proves it. Not a setting of ours; it is the owner's decision.
- **pinned** — the only thing the USER controls, and it governs **Home only**. Tabs, the browse
  grid, sort, the A–Z rail and every other browsing surface come from the grant; pinning decides
  whether a source's shelves merge into Home.
- **reachable** — a fact about NOW: something answered at one of its addresses, *as the right
  machine*. It changes while nobody touches anything, and it is never a reason to forget the grant
  or the pin.

Collapsing any two of them produces a specific wrong behaviour rather than a vague one. A `401` read
as "unreachable" sends the user to look at their friend's router when the fix is to refetch
`/resources` (every other address of that server will answer identically). An unreachable server
read as un-granted drops the source out of the Sources list, so there is nothing left to retry.
A pinned-but-dead source drawn as a shelf is a spinner that never ends — the design's answer is that
a dead source is **absent** from Home and states itself in its own library section instead.

## Gotchas that bite (all verified in code)

- **`Connection.local` does not mean what it looks like, and the cost is 8 seconds.** It means "this
  address is RFC1918", NOT "you are on that LAN" — a share advertises the *owner's* `172.20.x.x`.
  `publicAddressMatches` is the field that means the latter. `probe.rs` drops a `local` connection
  on a non-owned server unless that flag is set, because dialling it costs an 8 s timeout — and the
  worse outcome is that it SUCCEEDS, against a different machine of ours at that address. That is
  also why a probe must **verify `machineIdentifier` on the response** before accepting a
  connection, and why `probe.rs` is pure (no socket, no clock): the rules are then gradeable on the
  dev Mac, which is the only tier that can grade them at all.
- **A capped link is answered by the transcode FLAVOR, never by a bitrate parameter.** Plex's relay
  is a ~2 Mbit/s tunnel, so `transcoder::link_policy` denies direct play *and* the container remux
  over one — the remux is the half that is easy to miss, since it copies the codecs and deliberately
  sends no cap, i.e. the same bytes at the same rate one layer down. `TranscodeSpec` has no bitrate
  field and must not grow one. **No relay connection has ever been observed by this codebase**, so
  that policy is reasoned, not measured; read its doc before touching it.
- **Capture the server at the SPAWN SITE**, the rule the multi-server work inherits from
  `route::ResolveEnv` — whose doc says why in general: a worker reads no `static mut`, because the
  main thread reassigns those under it. "Which server is current" is exactly such a value, and a
  worker that asks *after* the user switched lands an answer belonging to the other machine's
  `ratingKey` space. Pass a `ServerId` (or the `&'static Client`) in with the job. One narrow
  exception exists today and is worth knowing rather than copying: `build_stream` reads
  `client_opt()` on the resolve worker. That is sound — the registry read is atomic and its clients
  are never freed — but it means "the current server", not "the server this play started from", and
  it is the line to change when a play can begin on a server that is not current.

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
