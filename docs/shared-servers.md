# Shared (non-owned) Plex servers — how Plex structures them, and how to integrate one

**Status:** design note, 2026-08-11. Written after being granted access to a friend's server.
Everything in §2 was **measured live** against that server, from the dev Mac *and* from the TV
itself. Everything in §1 is Plex's model as documented by its own client libraries. §5's plan is
sequenced, and steps 0 and 4a have landed (see §9).

Addresses, tokens, machine identifiers and the owner's username are deliberately **not** recorded
here — the same redaction rule `ui/stats.rs` applies to the diagnostics panel.

---

## 1. How Plex structures it

**The account is the index; each server is a separate authority.** Two APIs, two kinds of token.

`GET https://plex.tv/api/v2/resources?includeHttps=1&includeRelay=1&includeIPv6=1`, with the
**account** token, returns every server the account can reach — owned and shared alike:

| field | meaning |
|---|---|
| `clientIdentifier` | the server's `machineIdentifier`; the only stable identity |
| `owned` | `false` ⇒ shared with you |
| `sourceTitle` | the **owner's plex.tv username**. This is what "shared" looks like on the wire, and it is the label Plex's own TV client shows |
| `accessToken` | **per (user, server)** — carries the sharing grant |
| `httpsRequired`, `publicAddressMatches`, `presence`, `home`, `relay` | connection-policy inputs |
| `connections[]` | `{protocol, address, port, uri, local, relay, IPv6}` |

The account token authenticates you to **plex.tv only**. Every PMS request must carry that server's
own `accessToken` — it decides which libraries you see and where your watch state is written.
Plex's own clients (python-plexapi `MyPlexResource.connect`, plex-for-kodi `plexresource.py`) always
use the per-resource token, never the account token; for an owner the two merely happen to coincide.

**`plex.direct`.** A public CA will not issue for a private IP, so Plex runs a wildcard DNS zone:
`A-B-C-D.<hash>.plex.direct` resolves to `A.B.C.D`, and the server holds a real cert for
`*.<hash>.plex.direct`. That is why `connections[].uri` is an https URL with a dashed-IP hostname.
Use the `uri` **verbatim** — the `<hash>` label is the certificate UUID, *not* the
machineIdentifier. Connecting to the bare IP over https fails validation by design.

**Relay** is a tunnel the server holds open to a Plex relay host: another https connection, flagged
`relay:true`, conventionally port 8443, capped at **2 Mbps** with the server transcoding down to
fit. Last resort. The preference order in every client that has one is **local → remote → relay**.

**Everything item-shaped is per-server.** `ratingKey`, `librarySectionID`, `Part.key`, `Stream.id`,
`playQueueID`, personIds and image-transcode paths are all server-local integers starting at 1. The
only portable identity is the `guid` (`plex://movie/…`). Plex makes the scoping explicit in its own
grammar: a PlayQueue is created with
`uri=server://{machineIdentifier}/com.plexapp.plugins.library/library/metadata/{ratingKey}` —
which this repo already builds, at `rust-modules/src/plex/timeline.rs:56`.

**Consequences, all client-side:**

- PlayQueue creation, `/:/timeline`, `/:/scrobble`, `/decision` and `transcode_stop` must go to **the
  server the bytes came from**, with **that server's token**. `viewOffset` lives there.
- **Nothing aggregates server-side.** `/hubs`, `/hubs/continueWatching` and search are single-server;
  Plex's own provider contract describes `continuewatching` as a hub "for merging into a global
  Continue Watching hub" — the merge is the client's job.
- `/library/sections` returns only the granted sections. Owner-only surfaces (`/:/prefs`, `/butler`,
  `/activities`, deletion) return 403.
- The connection recipe: filter `provides ∋ server` → rank → probe candidates in parallel →
  **verify `machineIdentifier` on the probe response** before accepting it → treat `401` as its own
  state (token problem, refetch `/resources`) rather than "unreachable". python-plexapi additionally
  **drops every `local` connection on a non-owned resource** (`myplex.py`), and §2 shows exactly why.

---

## 2. What the actual shared server looks like (measured 2026-08-11)

`/api/v2/resources` for this account returns two servers: ours (`owned=true`) and the share
(`owned=false`, `sourceTitle` = the owner's username, `httpsRequired=false`, **no relay
connection**, per-server `accessToken` present). The share advertises three connections:

| connection | `local` | reachable from our LAN? |
|---|---|---|
| `172.20.x.x:32400` | **`true`** | **NO — 8 s timeout.** It is the *owner's* LAN address |
| `<custom hostname>:26937` | false | **NO — DNS does not resolve** (owner's internal name) |
| `<public IPv4>:26937` | false | **YES — 200 in 115 ms** |

Three findings, each load-bearing:

**(a) `local: true` is a trap.** The flag means "this address is RFC1918", not "*you* are on that
LAN" — `publicAddressMatches` is the field that means the latter, and it is `false` here. Our
current selector (`auth.rs:396`) filters on exactly `c.local && !c.relay`, so if it ever reached
this resource it would pick the owner's `172.20.x.x` and hang for 8 seconds — or, worse, reach a
*different machine* on our own LAN at that address. This is why the probe must verify
`machineIdentifier`, and why non-owned `local` connections should be dropped outright.

**(b) The per-server token is mandatory, and provably so.** Against the share's
`/library/sections`: our own server's token → **401**; a garbage token → **401**; the share's
`accessToken` → **200**. (`/identity` answers 200 to anything — it is unauthenticated, so it is
useless as a token test but perfect as a reachability probe.)

**(c) The reachable connection speaks plain HTTP on a numeric IPv4 address.** Verified from the
**TV itself**, not just the Mac:

```
# on the TV
wget -q -T 8 -O - http://<public-ip>:26937/identity
→ <MediaContainer size="0" … machineIdentifier="…" version="1.43.3"/>   in 1.2 s
```

That is precisely — and only — what `stream.rs` can already do: `AF_INET`, dotted quad, port from
the connection, plain HTTP, chunked supported. **For this server, transport is not the blocker.**

Also measured, because they shape the playback story:

- The share is **one movie section, 185 items** — and its section key is **`1`**, exactly like our
  own server's `Movies`. Section keys and ratingKeys collide across the two servers today.
- A representative item: **MKV, h264 + TrueHD, 1080p, 31 Mbit/s, 20 GB**. A range GET of the real
  part over the WAN link sustained **38.8 Mbit/s** — so a direct play of that remux fits, with
  almost no headroom, and TrueHD will force at least an audio transcode.
- The share's `/hubs` answers normally and is scoped to *our* account (Continue Watching is empty,
  correctly — we have watched nothing there).

---

## 3. What the app does today

**It already fetches the whole list, then throws it away.** `plex/account.rs:98-100` requests
`?includeHttps=1&includeRelay=1`. `Resource` (`account.rs:145-159`) keeps six fields and drops
`sourceTitle`, `ownerId`, `home`, `presence`, `publicAddressMatches`, `httpsRequired`.
`Connection.protocol` and `Connection.uri` are parsed and **never read** by anything.

**`owned` is already a preference, not a wall.** `auth.rs:389-400` tries owned servers first and
falls back to any server — the real filter is `c.local && !c.relay && !c.address.is_empty()` on
*both* passes (`auth.rs:396`). A remote server dies at `auth.rs:403` with *"No local Plex server
found on this network."* `Resource::local_connection()` (`account.rs:166-171`), whose lenient
fallback is the shape a remote server needs, is **dead code — zero callers**.

**One server, forever.** `plex/client.rs:149-162`:

```rust
static PLEX: OnceLock<Client> = OnceLock::new();
pub fn install(host: &str, port: i32, token: &str) {
    match PLEX.get() { None => { let _ = PLEX.set(Client::new(host, port, token)); }
                       Some(c) => c.set_token(token), } }
```

Host and port freeze at first install; a second `install` with a *different* server is silently
just a token swap. `client()` hands a `&'static Client` to ~30 sites outside `plex/`. **This
singleton, not the UI, is the feature.**

The globals that would collide across two servers — each an equality test or a bare index with no
server dimension:

| site | what collides |
|---|---|
| `pms.rs:66-68` `index_of_rk` | `position(\|m\| m.rk == rk)`; callers `app.rs:1664`, `app.rs:2801`, `ui/detail.rs:2754`, `:2761`. A detail page mounts the wrong item |
| `posters.rs:449-452` | every poster fetched from the singleton's host — and this bypasses `client.rs`, contradicting its own module doc |
| `posters.rs:124-128` | `KeyMemo` keyed `(path,w,h,png)`: no host, no token; flushed only on a *profile* switch |
| `metadata.rs:834-843` | `cached_playing(rk)` — server A's track list applied to server B's item |
| `metadata.rs:1291-1307` | `pump_season`'s `d.rk != r.rk` ownership test |
| `browse.rs:32-36` | `BrowseSection.key: i64` — **verified collision**: both servers have section `1` |
| `route.rs:35` | `MACHINE_ID`, "cached once", feeds the PlayQueue `server://` uri |
| `ui/trail.rs:42-59` | `Node::Detail{rk}` — navigation history itself is server-less |

Already server-agnostic, needing no work: `img.rs`, `player/engine.rs` + `threads.rs` (they consume
a full URL), `plex/discover.rs`, and the single `X-Plex-Client-Identifier` — one device on N servers
is *correct*; do not per-server it.

---

## 4. The blocker — and why it is smaller than it looks

The general answer is transport: `stream.rs:239-253` parses the host as a dotted quad by hand, so
DNS and TLS are both absent, and a `plex.direct` origin fails before a packet leaves the TV. Fixing
that properly means routing through libcurl (already bound, `net.rs:34-46`), which is ~1–1½ days for
the API + image lanes and a genuine **2–4 days** for the media lane, because FFmpeg's AVIO is *pull*
and curl is *push*, and `stream.rs`'s single-closer teardown protocol has to be re-earned in curl
terms. The bundled FFmpeg cannot help — it is built `--disable-network`,
`--enable-protocol=file` (`ci/build-ffmpeg.sh:122,129`) and pinned to majors 63/63/61.

**But none of that is on the critical path for the server we actually have.** §2(c) shows its usable
connection is plain HTTP on a numeric IP, reachable from the TV, with `httpsRequired=false`. So the
work that makes *this* share appear in the app is entirely **connection selection + the multi-server
data model** — no new transport at all.

Two caveats that make TLS a real follow-up rather than a nicety:

1. **Plain HTTP over the WAN puts `X-Plex-Token` in the clear**, in the query string, across the
   public internet — and it is written into the *owner's* PMS access log. Fine for a personal build
   today; not something to ship broadly.
2. The plain-HTTP route is **the owner's setting, not ours**. If they flip *Require secure
   connections* to Required, or their port-forward stops exposing the plain port, it disappears and
   only the curl path reaches them. Same for any share that is relay-only.

---

## 5. The plan

Each step compiles, passes `make check`, and ships alone. Two standing hazards: `dynlib::load_into`
is all-or-nothing, so one missing libcurl symbol sets `CURL_OK=false` and kills plex.tv sign-in —
probe first, and put optional symbols in a **second** `dynlib!` table. And `plex/client.rs` has no
test module at all, so any step touching `StreamUrl::parse` must bring its own tests.

| # | Step | Effort | What it buys |
|---|---|---|---|
| 0 | **Parse what plex.tv already sends.** Widen `Resource`/`Connection` (`account.rs:145-190`) with `sourceTitle`, `ownerId`, `home`, `presence`, `publicAddressMatches`, `httpsRequired`, `IPv6`; add `includeIPv6=1`; log one line per resource + connection. Selection untouched. **Every new string must be `Option<String>`** — plex.tv sends explicit `null`, serde's `default` does not cover it, and one nullable field fails the whole parse and kills sign-in (`account.rs:209-212` already records this trap). | ½ d | Observation |
| 1 | **Server registry, one slot.** New `plex/servers.rs`: `ServerId(u16)`, `Server`, `Conn{scheme,…}`, a `CLIENTS` table + `CURRENT`. `install` registers slot 1; `client()`/`client_opt()` keep their signatures and now mean "the current server". `TOKEN_GEN` moves **into** `Client`. Zero call-site changes. Note `client()` is hot — `posters::poster_key` calls it three times per key per tile per frame, so use an atomic-pointer table, not an `RwLock`. | ½–1 d | Foundation |
| 2 | **Thread `ServerId` through the stored structs** — `PmsMovie`, `BrowseSection`, `Detail`, `PlayingItem`, `Person`, `Pslot`, `trail::Node`, `ResolveEnv`/`Plan`, `UpNext`/`QueueRow`. Every rk equality test becomes a pair. The rule is **capture at the spawn site**, never read the current server inside a worker (`browse.rs:576` says so explicitly; `ResolveEnv` is the template). Behaviour change: none. | 2–3 d | The mechanical diff |
| 3 | **Move the ~30 call sites onto `client_for(sid)`.** `posters.rs:452` becomes `client_for(slot.sid)?.fetch_built(&key)` — **token-free**, because the poster key already ends in `with_token(…)` and `get_bytes` would append a second one. Ship gate: byte-identical event log across `tests/run.py`. | 1 d | Correctness |
| 4 | **Probe + race.** New `plex/probe.rs`: drop `local` on `!owned` unless `publicAddressMatches` (§2a); drop http when `httpsRequired`; otherwise synthesize `http://{addr}:{port}` **— this is the step that makes the current share work**; rank local→remote→relay; probe in parallel; **verify `machineIdentifier`**; treat 401 as its own state. `auth.rs:379-418` becomes `ingest()` + `activate_best()`, keeping today's scan as a fallback. Pump it where `pms::pump` lives (route-gated), not beside `route::pump_play`, and call `ui::idle::invalidate()` on any landing that repaints. | 1–1½ d | **The share becomes reachable** |
| 5 | **Persist the registry; boot from the hint.** `session.rs` gains `servers: Vec<ServerRec>` + `current_machine_id`, every field `#[serde(default)]`, legacy `ServerRef` still written for one release. A corrupt `servers` array must not fail the whole `Session` parse — that is a silent sign-out at every boot. No timestamps: this TV's wall clock is ~3 h skewed. | 1 d | Fast boot |
| 6 | **TLS control plane.** New `http.rs` façade — `Scheme::Http` → `stream.rs` unchanged, `Scheme::Https` → curl. Generalise `net.rs:131` `perform`: per-call timeout (its `TIMEOUT=25` is right for an API call and fatal for media), `CUSTOMREQUEST` for the PUT verb it has never had, persistent handle **only if** `HTTPHEADER`/`POSTFIELDS`/`POST` are explicitly cleared each call (`auth.rs:285` is a header-less call that would otherwise read freed memory). | 1–1½ d | Any https-only share browses |
| 7 | **TLS media plane.** Second `dynlib!` table (`curl_multi_*`, all probed PRESENT); `AvioState` gains a source enum; `read_cb`/`seek_cb` dispatch; pump `curl_multi` **from inside `read_cb`** so the demux thread self-polls its abort flag and teardown collapses to "set the flag, join". Preserve the seek abort guard (`ff.rs:1288`) verbatim. The two existing abort-guard tests construct `AvioState` by literal, so this breaks them at compile time — extend them. | 2–4 d | Any share plays |
| 8 | **N servers live** — the Sources list as a library-toolbar chip with a two-level panel (§6), `sourceTitle` as the row subtitle, attribution in **text not artwork** (a corner badge fails the one-mark-per-tile rule — see §6). Four structural hazards: `install_pms` fetches hubs and sections **blocking on the main thread**, so fanning out over N servers freezes boot; `pms::fetch_build` is an all-or-nothing `?` chain, so one dead share blanks Home; `ensure_sections` early-returns while non-empty, which is the only thing keeping the page mailbox sound; and `install_pms` must split into *identity change* (wipe all) vs *server activation* (wipe nothing). Continue Watching is the one shelf that should merge (by `lastViewedAt`, which `pms.rs:307` already sorts). | 2–4 d | The product |
| 9 | **Relay policy.** There is no bitrate field to clamp: `TranscodeSpec` has none and `maxVideoBitrate` is a literal on the re-encode branch only. Respecting relay's 2 Mbps means **forcing a transcode decision** in `build_stream` — a policy change, not a parameter. | ½–1 d | Correctness on relay |

**Shortest path to seeing the share on screen: 0 → 1 → 4**, plus enough of 2/3 to keep the caches
honest. Steps 6–7 are what make it robust for *any* share rather than this one.

**The cheap variant**, named honestly: if one **active** server at a time is acceptable (switch,
never merge), steps 2, 3 and most of 8 shrink to a picker that does a full identity-style reset on
every switch — roughly a third of the diff. What it costs: no merged Home or Continue Watching, and
switching throws away the other server's grid, scroll and focus every time. It still needs step 1,
because `OnceLock` cannot re-point at all.

---

## 6. How it appears in the UI — SUPERSEDED by the design

**The design team answered this brief on 2026-08-13 and changed three of its five deliverables.**
The canvas (`Shared Sources.dc.html`, project `3ec1f4af…`) is the source of truth; what follows is
only its shape, so this doc does not point the next implementer the wrong way.

**With one source, none of it is drawn** — not a bare suffix, not an empty slot. The Sources row is
not built, the strip has three pills, the headings carry no annotation.

**People in content, machines in settings.** The handle (`bamx23`) on every browsing surface; the
machine name (`bx23-ldn`) only in the Sources list and the failure read-out.

- **A — the Sources list is a LIBRARY TOOLBAR CHIP**, not a row in the account popover. `Library ·
  LDN Films  bamx23 ▾`, opening a 640-wide panel with **two levels** switched by Browse / On Home
  pills at the panel top (the track menu's own swap). **Browse** is a picker — one tick, OK closes.
  **On Home** is a toggle — the word `On`/`Off` at the trailing edge, OK flips, the panel stays open.
  Grouped by server: header = machine, accessory = person. The last pinned library uses `value_dim`;
  an unreachable server's whole group dims at .52, header included. "Check for new shares" sits last
  under a separator. Rejecting the popover also withdraws both of the flags this doc raised about
  `account_menu.rs`'s static arrays and its close-before-acting OK.
- **B — the tab strip carries NOTHING new.** A pill is a **type**, always bare; it grows by missing
  types (a friend sharing Music you do not own), never by people, so it is 447px constant at any
  number of friends. The width-map flag is withdrawn — the strip never sees an annotation.
- **C — source lives in the shelf heading**: `Recently Added in LDN Films · bamx23`, one rung down,
  regular against bold, tertiary, after a middot at .45. **Continue Watching merges across pinned
  sources and carries no annotation at all** — a shelf drawn from three servers cannot be named by
  one of them. Nothing on the tile, ever. The hero needs nothing: it *is* the shelf's focused tile.
- **D — a dead source is absent from Home** (no shelf, no spinner) and its borrowed items leave
  Continue Watching. Its library section draws the shared failure read-out: `Can't reach bx23-ldn`,
  reason `Shared by bamx23 · your own server is fine.`, one action `Try again`, anchored in the
  content region with only the Source chip beside it — no sort/filter chips, no count, no A–Z rail.
- **E — `Shared by bamx23`** as the last run on the detail hero's date/runtime line, plus an **Also
  available** button in the actions row when a second pinned source holds the film. **OK navigates**
  to that server's page rather than swapping the copy in place, which is also what settles the
  per-server resume position.
- **F — a first-run route** (new): after the profile picker, before Home, only when the roster holds
  more than one source. Two columns, own libraries `On` and a friend's `Off`, focus on *Start
  watching*, BACK skips.

**PINNING is the new concept.** It governs **Home only** — tabs, grid, sort, A–Z rail and browsing
all come from the grant, which is not a setting. Three orthogonal states: *granted* (plex.tv's
answer), *pinned* (the only control), *reachable* (a fact about now).

**Two divergences between the canvas and main, both because main moved while it was drawn**, neither
requiring the design to change: its toolbar frames include an `Unwatched` chip that `0d9a4f6f`
deleted (the toolbar is two chips today, so Source makes **three**), and its rejected-list reasons
from "the unwatched angle", which `23f28ce6` replaced with the white tick over a veil.

---

## 7. Open questions

1. **Playback of the share's content, end to end.** Untested. The sample item is h264 + **TrueHD**
   at 31 Mbit/s; TrueHD is not in the direct-play audio set, so this goes down the transcode path
   over a WAN link measured at 38.8 Mbit/s. Expect this, not direct play, to be the common case —
   and it argues for a remote-aware bitrate policy well before relay does (step 9).
2. **The harness cannot grade any of this yet.** `/tmp/plxnative-token` carries exactly one token,
   so a second server's `accessToken` cannot be injected headlessly. Steps 4–8 are not gradeable by
   `tests/run.py` until that overlay grows a second entry.
3. **plex.direct DNS from inside the app's jail** (curl uses c-ares; the jail's resolv view differs
   from the ssh shell's). Prove by logging the resolved address on the first remote request.
4. **TLS on 256 KB stacks, concurrently** — `task::spawn_small`'s stack, with seven worker kinds
   each doing a handshake. Device-verify under a full Home + library scroll.
5. **Relay end to end** has never been observed by this codebase: `includeRelay=1` has been
   requested forever and every relay connection unconditionally discarded. The 2 Mbps cap and port
   8443 are documentation, not measurement.

## 8. Doc corrections this work turned up

All confirmed in code, all worth fixing at the source so the next reader is not misled:

- Root `CLAUDE.md` says `stream.rs` has "no chunked decoding". It has had chunked since
  `stream.rs:116-147` / `:368-421`.
- Root `CLAUDE.md` and `player/CLAUDE.md` describe `ff.rs` as "the TV's own libavformat", dlopen'd
  by SONAME candidate list. FFmpeg is **bundled and pinned** (majors 63/63/61, `-plx` suffix,
  loaded by absolute path). This is what makes "use FFmpeg's https protocol" a decided question
  rather than an open one.
- Root `CLAUDE.md` still claims the dual-FFmpeg-header ABI gate.
- The host suite is **396** tests as of 2026-08-13 (386 before this note's two units), not the 284 recorded.
  Re-derive it rather than trusting any number written down: `cargo test --lib -- --list | grep -c ': test'`.

---

## 9. What has landed (2026-08-13)

Two host-testable units, no behaviour change, `make check` at **396 passed**. Neither has a caller
yet: the live selector at `auth.rs:390-400` still filters on `c.local && !c.relay` on both passes, so
the 8-second trap of §2(a) is **still live in the running app**. This is foundation, not the feature.

- **`plex/account.rs` — the roster DTOs widened** to `sourceTitle`, `ownerId`, `home`, `presence`,
  `publicAddressMatches`, `httpsRequired` and `Connection.IPv6`, with `includeIPv6=1` on the query.
  Every field is now null-tolerant — `sourceTitle` a real `Option`, the other strings through
  `de_str`, `connections` through `de_vec`, the flags through a new `de_bool`, the ids through
  `de_i64` — because `#[serde(default)]` covers an *absent* field and not a present `null`, and one
  strict field meeting one null fails the whole array and ends sign-in at "no server found".
  `Resource::local_connection()` (dead, zero callers) is deleted; `probe.rs` supersedes it.
- **`plex/probe.rs` — the connection policy, pure.** No socket, no thread, no clock, so all of it is
  host-testable on Darwin. Builds and ranks candidate origins: drops a `local` connection on a
  non-owned server unless `publicAddressMatches`, suppresses every plain-http candidate when the
  owner set `httpsRequired`, and ranks local → remote → relay. It also carries, as doc, the two rules
  a real prober must honour and this module cannot: verify `machineIdentifier` on the response before
  accepting a connection, and treat `401` as its own state rather than "unreachable".

Two of the tests are worth knowing about, because both were written after a mutation showed the
suite could not see the bug:

- The owned-server fixture carries **`publicAddressMatches: false`** — the value the live capture
  actually returns. With `true` there, deleting `&& !res.owned` from the drop rule passed the entire
  suite while, in the field, discarding **our own** `192.168.x.x` and offline play with it.
- An explicit `null` on every string and on `connections` itself is asserted to cost that field and
  never the roster — the second server in that fixture is a good one, and the test is really about
  it still arriving.

## 10. The section table goes multi-server, and gets its Source chip (deliverable A)

Step 1's registry now has its first real consumer, and deliverable A of the design is drawn.

- **The section table addresses (SOURCE, section).** `BrowseSection` carries the source its row came
  from, and `BrowseSource` is the granted roster projected out of `plex::server_ids()` — the §3 table's
  verified collision (both servers have a section `1`) is what the address closes. Every fetch goes
  through `client_for(sid)` **captured at the spawn site**; nothing reads `client()` inside a worker.
- **It grows by APPEND, never by rebuild**, and that is the whole soundness argument. A page landing
  is blamed on a section INDEX, so a table that reshuffled under an in-flight fetch would splice one
  library's items into another's store. `ensure_sections`'s early-return used to be the only thing
  preventing that; appending replaces it and holds for every source, not only for the second call.
  Two generations now, each with a crisp job: `SECTIONS_GEN` (shape — bumped by an append, what the
  label caches key on) and `EPOCH` (identity — bumped by `reset` alone, what index-blamed landings
  gate on, so one source's append cannot discard another's answer).
- **Only the CURRENT server is discovered on the main thread**, which is the §5 step-8 hazard closed:
  `ensure_sections` runs at boot and on every Library entry, so fanning out there would park the SDL
  loop for one `connect(2)` per unreachable share. Every other source is discovered by a worker off
  `pump` — sections, then the server's own `friendlyName`, then a `size=0` count probe per library —
  with a 10 s per-source backoff. A dead share arrives as `reachable: false` and costs nothing.
- **Pinning** is the design's one control and governs Home only: your own libraries start pinned, a
  friend's start unpinned, and the last pinned one cannot be turned off. `pinned_libraries()` is its
  read side, waiting for deliverable C.
- **The tab strip is deliberately unchanged** (deliverable B, "no new drawing"): `browse::tab_*`
  projects the table to one pill per TYPE — your own sections, plus any type only a friend has — so
  the strip is a constant width at one friend or at ten, and the selection capsule for a borrowed
  library rests on its type's pill. With one source it is the identity map.
- **The Source chip and its two-level panel** are `ui/library.rs`; the row model is pure and
  host-tested. `TableView` gained the two things it was missing for it: a drawn `Section::accessory`
  (declared but never painted before) and `Section::dim`.
- **The roster's own facts** (machine name, owner handle, owned) live beside the registry as
  `plex::ServerFacts`, merged rather than replaced so plex.tv and a server naming itself over `GET /`
  can land in either order. `/tmp/plxnative-servers` gained a `handle` field, and `run.py` fills it
  from the resource's `sourceTitle`, so a two-source run is gradeable headlessly.

- **Picking a library on another server MOVES the app's current server** (`browse::set_cur` →
  `activate_source_of`), which is §5's named *cheap variant* — one active server at a time — and it
  is here because without it the Sources list is a trap. `PmsMovie` carries no `ServerId`, so a
  borrowed card's poster is fetched from `client()` and its ratingKey is resolved through `client()`;
  ratingKeys are server-local, so OK on a friend's card would quietly open, and PLAY, a different
  title of yours with the same number. Moving `current` makes all of them agree. What it costs is
  stated rather than hidden: Home's catalog belongs to the server it came from, so it is dropped and
  re-armed (`pms::reset`; `pms::pump` refetches asynchronously), the person page's shelves with it,
  and `route`'s cached `machineIdentifier` is forgotten so the next PlayQueue names the right
  machine. The poster memo needs no help — it compares a token generation and two servers never
  share one.

**Still open, and what retires that seam:** threading `ServerId` through `PmsMovie` and the other
stored structs (§5 step 2) and moving the ~30 call sites onto `client_for` (step 3). Until then Home
is single-source, which is also why deliverable C's merged shelves are not drawn.

**Not verified on device.** The host suite is 431 and `make` (ARM, dev and RELEASE) links, but every
screen described above is drawn by a television nobody had while this landed. The PR carries the
recipe.
