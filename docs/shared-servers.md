# Shared (non-owned) Plex servers — how Plex structures them, and how to integrate one

**Status:** design note, 2026-08-11; landed-work section refreshed 2026-08-13. Written after being
granted access to a friend's server. Everything in §2 was **measured live** against that server,
from the dev Mac *and* from the TV itself. Everything in §1 is Plex's model as documented by its own
client libraries. §5's plan is sequenced; **§9 is the record of what has actually landed**, and it
is the section to read before starting a step — several of them are now partly done, and one (the
relay policy, step 9) is done ahead of the transport work that can exercise it.

**Anonymisation — read this before adding an example anywhere in the repo.** Addresses, ports,
tokens, machine identifiers, the owner's username and their library names are deliberately **not**
recorded here or in any fixture, doc comment, commit message or PR body — the same redaction rule
`ui/stats.rs` applies to the diagnostics panel, and for a stronger reason: **this repository is
public, and none of that data is ours.** It belongs to the person who shared their server.

This paragraph stood here, in these words, while the repo published the friend's handle, their
machine name, their library name, their real port and their LAN address across ~139 sites in
committed code and four PR bodies (2026-08-14). Stating a rule is not applying it. The stand-ins
now used throughout — and the only ones to use in new work — are:

| real thing | stand-in |
|---|---|
| owner's plex.tv handle | `friend` |
| server / machine name | `nas-home` |
| their library name | `Film Club` |
| their port | `31234` |
| their LAN address | `10.9.9.7` (RFC1918, as the real one is) |
| any public address | `203.0.113.9` / `198.51.100.7` (TEST-NET-3 / TEST-NET-2) |
| a machine identifier | `aaaabbbb…` runs, never a real 40-hex id |

Several are deliberately the **same character length** as what they replaced, because `ui/home.rs`
asserts text widths against them. The live values live only in the gitignored
`tests/manifest.local.json` and `src/config.local.h`, which is the whole reason those files are
gitignored.

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
`A-B-C-D.<label>.plex.direct` resolves to `A.B.C.D`, and the server holds a real cert for
`*.<label>.plex.direct`. That is why `connections[].uri` is an https URL with a dashed-IP hostname.
Connecting to the bare IP over https fails validation by design.

**THE RULE IS: use the advertised `uri` VERBATIM, and never construct the hostname.** That is the
whole of what a client needs, it is what the official client does, and it is correct under either
answer to the question below — which is why it is stated first and separately.

**What the `<label>` actually is, is genuinely disputed, and this file used to pick a side without
saying so.** It read *"the `<hash>` label is the certificate UUID, **not** the machineIdentifier"*,
flatly. Meanwhile `docs/plex-openapi.json`'s `servers[0]` describes the very same label — its
`identifier` path variable in `https://{IP-description}.{identifier}.plex.direct:{port}` — as
*"The unique identifier of this particular PMS"*, with a 32-hex default, which reads as the
machineIdentifier. **Two sources in this repo disagree and neither is graded above the other:**
one is a note written from observation, the other is a published OpenAPI description; the shapes
are indistinguishable, because a machineIdentifier and a dashless UUID are both 32 hex characters,
so no sample settles it by inspection. It is left recorded as a disagreement rather than resolved,
because resolving it would take a server whose `machineIdentifier` is known and whose advertised
`uri` can be compared against it character by character, and nobody has done that.

**Neither answer changes what a client does**, which is the point. The official Plex client never
builds this hostname from an identifier it holds: it **regex-captures the label out of a `uri` the
resources API already gave it**, and the only hostname it ever assembles itself is the loopback form
`127-0-0-1.<label>.plex.direct` — where the label is again one it extracted, not one it derived. So
the safe rule survives the ambiguity intact: **the label is data you copy, never data you compute.**
Treat any code that would build `plex.direct` out of a `clientIdentifier` as a bug even if the
OpenAPI reading turns out to be the right one, because the failure mode is a TLS validation error
against a certificate you cannot inspect from the TV.

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
  server the bytes came from**, with **that server's token**. `viewOffset` lives there. (One
  deliberate exception, and only for the watched FLAG: a Mark as Watched is repeated on every source
  holding the same `guid` — §11. Nothing else here fans out, and no `viewOffset` ever does.)
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
| `<custom hostname>:31234` | false | **NO — DNS does not resolve** (owner's internal name) |
| `<public IPv4>:31234` | false | **YES — 200 in 115 ms** |

Three findings, each load-bearing:

**(a) `local: true` is a trap.** The flag means "this address is RFC1918", not "*you* are on that
LAN" — `publicAddressMatches` is the field that means the latter, and it is `false` here. Our
current selector (`auth::choose_local_connection`) filters on `c.local && !c.relay` (plus an IPv4
guard added 2026-08-14, §9), so if it ever reached this resource it would pick the owner's
`172.20.x.x` and hang for 8 seconds — or, worse, reach a *different machine* on our own LAN at that
address. This is why the probe must verify
`machineIdentifier`, and why non-owned `local` connections should be dropped outright.

**(b) The per-server token is mandatory, and provably so.** Against the share's
`/library/sections`: our own server's token → **401**; a garbage token → **401**; the share's
`accessToken` → **200**. (`/identity` answers 200 to anything — it is unauthenticated, so it is
useless as a token test but perfect as a reachability probe.)

**(c) The reachable connection speaks plain HTTP on a numeric IPv4 address.** Verified from the
**TV itself**, not just the Mac:

```
# on the TV
wget -q -T 8 -O - http://<public-ip>:31234/identity
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

## 3. What the app did on 2026-08-11 — and what of it is still true

*The first three paragraphs describe the app as this note found it. Two of them have since been
answered (§9); they are kept because the collision table below is only readable against them.*

**It already fetched the whole list, then threw it away.** `plex/account.rs` requested
`?includeHttps=1&includeRelay=1`; `Resource` kept six fields and dropped `sourceTitle`, `ownerId`,
`home`, `presence`, `publicAddressMatches`, `httpsRequired`; `Connection.protocol` and
`Connection.uri` were parsed and never read. **Fixed** — the roster DTOs are widened and
null-tolerant (§9).

**`owned` is a preference, not a wall — and this part is STILL LIVE.** `auth.rs` tries owned servers
first and falls back to any server, but the real filter is `c.local && !c.relay &&
!c.address.is_empty()` on *both* passes (now `auth::choose_local_connection`, plus the IPv4 guard of
§9), so a remote-only server dies with *"no server with a local connection (remote-only can't be
reached)"*. `probe.rs` knows better and nothing calls it
yet: **the 8-second trap of §2(a) is still what the running app would do.** This line is step 4's
whole reason to exist.

**One server, forever — FIXED.** `plex/client.rs` held a `static PLEX: OnceLock<Client>` whose host
and port froze at the first `install`, so a second `install` naming a *different* server was
silently just a token swap against the first one's address — a mis-target no call site could see.
`servers.rs` replaced it with a registry keyed on `machineIdentifier` (§9); `client()` still hands a
`&'static Client` to ~30 sites outside `plex/` and now means "the current server".

The globals that would collide across two servers — each an equality test or a bare index with no
server dimension. **Line numbers are as of 2026-08-11 and have moved; the entries themselves were
re-verified 2026-08-13 and all but one still hold:**

| site | what collides |
|---|---|
| `pms.rs:66-68` `index_of_rk` | `position(\|m\| m.rk == rk)`; callers `app.rs:1664`, `app.rs:2801`, `ui/detail.rs:2754`, `:2761`. A detail page mounts the wrong item |
| `posters.rs:449-452` | every poster fetched from the singleton's host — and this bypasses `client.rs`, contradicting its own module doc |
| `posters.rs:124-128` | `KeyMemo` keyed `(path,w,h,png)`: no host, no token. **Half-fixed:** it still carries no host, but token generations are now unique per `Client`, so the memo also flushes when `client()` starts answering with a different server — it used to flush only on a *profile* switch, which would have served server B its cards from A's memoised paths |
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

**Status is in the first cell of each row, and §9 is the account.** A step marked LANDED is done as
described unless the cell says otherwise; the plan text itself is left as written, because what it
asked for is how to read what shipped.

| # | Step | Effort | What it buys |
|---|---|---|---|
| 0 **LANDED** | **Parse what plex.tv already sends.** Widen `Resource`/`Connection` (`account.rs:145-190`) with `sourceTitle`, `ownerId`, `home`, `presence`, `publicAddressMatches`, `httpsRequired`, `IPv6`; add `includeIPv6=1`; log one line per resource + connection. Selection untouched. **Every new string must be `Option<String>`** — plex.tv sends explicit `null`, serde's `default` does not cover it, and one nullable field fails the whole parse and kills sign-in (`account.rs:209-212` already records this trap). | ½ d | Observation |
| 1 **LANDED** | **Server registry** — shipped with N slots rather than the one this row asked for; the ceiling is `MAX_SERVERS = 16`. New `plex/servers.rs`: `ServerId(u16)`, `Server`, `Conn{scheme,…}`, a `CLIENTS` table + `CURRENT`. `install` registers slot 1; `client()`/`client_opt()` keep their signatures and now mean "the current server". `TOKEN_GEN` moves **into** `Client`. Zero call-site changes. Note `client()` is hot — `posters::poster_key` calls it three times per key per tile per frame, so use an atomic-pointer table, not an `RwLock`. | ½–1 d | Foundation |
| 2 | **Thread `ServerId` through the stored structs** — `PmsMovie`, `BrowseSection`, `Detail`, `PlayingItem`, `Person`, `Pslot`, `trail::Node`, `ResolveEnv`/`Plan`, `UpNext`/`QueueRow`. Every rk equality test becomes a pair. The rule is **capture at the spawn site**, never read the current server inside a worker (`ResolveEnv`'s doc is the template and gives the general form of it; the `browse.rs:576` citation this row gave does not survive — that line has moved and carries no such comment). Behaviour change: none. | 2–3 d | The mechanical diff |
| 3 | **Move the ~30 call sites onto `client_for(sid)`.** `posters.rs:452` becomes `client_for(slot.sid)?.fetch_built(&key)` — **token-free**, because the poster key already ends in `with_token(…)` and `get_bytes` would append a second one. Ship gate: byte-identical event log across `tests/run.py`. | 1 d | Correctness |
| 4 **HALF LANDED — the policy, not the race** | **Probe + race.** New `plex/probe.rs`: drop `local` on `!owned` unless `publicAddressMatches` (§2a); drop http when `httpsRequired`; otherwise synthesize `http://{addr}:{port}` **— this is the step that makes the current share work**; rank local→remote→relay; probe in parallel; **verify `machineIdentifier`**; treat 401 as its own state. `auth.rs:379-418` becomes `ingest()` + `activate_best()`, keeping today's scan as a fallback. Pump it where `pms::pump` lives (route-gated), not beside `route::pump_play`, and call `ui::idle::invalidate()` on any landing that repaints. | 1–1½ d | **The share becomes reachable** |
| 5 | **Persist the registry; boot from the hint.** `session.rs` gains `servers: Vec<ServerRec>` + `current_machine_id`, every field `#[serde(default)]`, legacy `ServerRef` still written for one release. A corrupt `servers` array must not fail the whole `Session` parse — that is a silent sign-out at every boot. No timestamps: this TV's wall clock is ~3 h skewed. | 1 d | Fast boot |
| 6 | **TLS control plane.** New `http.rs` façade — `Scheme::Http` → `stream.rs` unchanged, `Scheme::Https` → curl. Generalise `net.rs:131` `perform`: per-call timeout (its `TIMEOUT=25` is right for an API call and fatal for media), `CUSTOMREQUEST` for the PUT verb it has never had, persistent handle **only if** `HTTPHEADER`/`POSTFIELDS`/`POST` are explicitly cleared each call (`auth.rs:285` is a header-less call that would otherwise read freed memory). | 1–1½ d | Any https-only share browses |
| 7 | **TLS media plane.** Second `dynlib!` table (`curl_multi_*`, all probed PRESENT); `AvioState` gains a source enum; `read_cb`/`seek_cb` dispatch; pump `curl_multi` **from inside `read_cb`** so the demux thread self-polls its abort flag and teardown collapses to "set the flag, join". Preserve the seek abort guard (`ff.rs:1288`) verbatim. The two existing abort-guard tests construct `AvioState` by literal, so this breaks them at compile time — extend them. | 2–4 d | Any share plays |
| 8 **STARTED** — the shelf heading can name a source (deliverable C), and `HubRow.source` is `""` at both construction sites, so nothing is reachable until this step populates it | **N servers live** — the Sources list as a library-toolbar chip with a two-level panel (§6), `sourceTitle` as the row subtitle, attribution in **text not artwork** (a corner badge fails the one-mark-per-tile rule — see §6). Four structural hazards: `install_pms` fetches hubs and sections **blocking on the main thread**, so fanning out over N servers freezes boot; `pms::fetch_build` is an all-or-nothing `?` chain, so one dead share blanks Home; `ensure_sections` early-returns while non-empty, which is the only thing keeping the page mailbox sound; and `install_pms` must split into *identity change* (wipe all) vs *server activation* (wipe nothing). Continue Watching is the one shelf that should merge (by `lastViewedAt`, which `pms.rs:307` already sorts). | 2–4 d | The product |
| 9 **LANDED, UNVERIFIABLE** | **Relay policy.** There is no bitrate field to clamp: `TranscodeSpec` has none and `maxVideoBitrate` is a literal on the re-encode branch only. Respecting relay's 2 Mbps means **forcing a transcode decision** in `build_stream` — a policy change, not a parameter. | ½–1 d | Correctness on relay |

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
The canvas (`Shared Sources.dc.html`, project `3ec1f4af…`) is the source of truth — **open it before
building any of this**; what follows is only its shape, kept here so this doc does not point the
next implementer the wrong way. Where the two disagree, the canvas wins. In particular the earlier
draft of this section put the Sources list in the **account popover** and gave the **tab strip** a
per-source annotation, and both are wrong: it is a library-toolbar chip, and the strip carries
nothing new at any number of friends.

**Deliverable C is the one with code behind it** (§9): a shelf heading can already name its source,
and does so as a second text run on the same painter, absent rather than empty when there is none.

**With one source, none of it is drawn** — not a bare suffix, not an empty slot. The Sources row is
not built, the strip has three pills, the headings carry no annotation.

**People in content, machines in settings.** The handle (`friend`) on every browsing surface; the
machine name (`nas-home`) only in the Sources list and the failure read-out.

- **A — the Sources list is a LIBRARY TOOLBAR CHIP**, not a row in the account popover. `Library ·
  Film Club  friend ▾`, opening a 640-wide panel with **two levels** switched by Browse / On Home
  pills at the panel top (the track menu's own swap). **Browse** is a picker — one tick, OK closes.
  **On Home** is a toggle — the word `On`/`Off` at the trailing edge, OK flips, the panel stays open.
  Grouped by server: header = machine, accessory = person. The last pinned library uses `value_dim`;
  an unreachable server's whole group dims at .52, header included. "Check for new shares" sits last
  under a separator. Rejecting the popover also withdraws both of the flags this doc raised about
  `account_menu.rs`'s static arrays and its close-before-acting OK.
- **B — the tab strip carries NOTHING new.** A pill is a **type**, always bare; it grows by missing
  types (a friend sharing Music you do not own), never by people, so it is 447px constant at any
  number of friends. The width-map flag is withdrawn — the strip never sees an annotation.
- **C — source lives in the shelf heading**: `Recently Added in Film Club · friend`, one rung down,
  regular against bold, tertiary, after a middot at .45. **Continue Watching merges across pinned
  sources and carries no annotation at all** — a shelf drawn from three servers cannot be named by
  one of them. Nothing on the tile, ever. The hero needs nothing: it *is* the shelf's focused tile.
- **D — a dead source is absent from Home** (no shelf, no spinner) and its borrowed items leave
  Continue Watching. Its library section draws the shared failure read-out: `Can't reach nas-home`,
  reason `Shared by friend · your own server is fine.`, one action `Try again`, anchored in the
  content region with only the Source chip beside it — no sort/filter chips, no count, no A–Z rail.
- **E — `Shared by friend`** as the last run on the detail hero's date/runtime line, plus an **Also
  available** button in the actions row when a second pinned source holds the film. **OK navigates**
  to that server's page rather than swapping the copy in place, which is also what settles the
  per-server resume position.
- **F — a first-run route** (new): after the profile picker, before Home, only when the roster holds
  more than one source. Two columns, own libraries `On` and a friend's `Off`, focus on *Start
  watching*, BACK skips. **LANDED — see §12, which also records the one place the OWNER's ruling
  overrides this canvas: the selection is per Plex Home PROFILE, not per install.**

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
2. ~~**The harness cannot grade any of this yet.**~~ **ANSWERED** (§9): `/tmp/plxnative-servers`
   carries a JSON array of ADDITIONAL servers — host, port, machineIdentifier and the token to trust
   them with — beside the unchanged `/tmp/plxnative-token`, and `run.py` resolves the second token
   from `/api/v2/resources` so no new secret is stored. One limitation stands and is not a bug to
   fix: there is **no managed-user token for someone else's server**, so a case that PLAYS from a
   share plays as YOU there. `test_user` isolation stops at the account boundary.
3. **plex.direct DNS from inside the app's jail** (curl uses c-ares; the jail's resolv view differs
   from the ssh shell's). Prove by logging the resolved address on the first remote request.
4. **TLS on 256 KB stacks, concurrently** — `task::spawn_small`'s stack, with seven worker kinds
   each doing a handshake. Device-verify under a full Home + library scroll.
5. **Relay end to end** has never been observed by this codebase: `includeRelay=1` has been
   requested forever and every relay connection unconditionally discarded (`auth.rs:396`). The
   2 Mbps cap and port 8443 are documentation, not measurement. **A policy now exists in code
   anyway** (`plex::link_policy`, §9) and is unverified for exactly this reason — and it cannot be
   verified from here even deliberately: **this account's share advertises no relay connection at
   all** (§2), so there is nothing to dial. Confirming it needs a server that is genuinely
   relay-only — an owner behind CGNAT, or one who turns their port forward off for an afternoon.

## 8. Doc corrections this work turned up — and where they stand

All were confirmed in code. **All four are now fixed at the source**, which is the point of listing
them; they are kept here as a record of what the wrong text was costing, because that is the part a
re-read cannot recover.

- Root `CLAUDE.md` said `stream.rs` had "no chunked decoding". It has decoded chunked since
  `HttpStream`'s `chunked`/`chunk_left` and `hs_next_chunk`. **Fixed.** Left alone it made
  `stream.rs` read as less capable than it is, and sent work to `net.rs`/curl that the raw socket
  could already do.
- Root `CLAUDE.md` and `player/CLAUDE.md` described `ff.rs` as "the TV's own libavformat", dlopen'd
  by SONAME candidate list. FFmpeg is **bundled and pinned** (majors 63/63/61, `-plx` suffix, opened
  by absolute path out of `paths::app_dir()`). **Fixed in both** — and on 2026-08-13 also in the
  **Makefile itself**, whose `LIBS_REAL` comment still filed FFmpeg beside curl and ACB as "SONAME
  moves, 55→57→58→59→60", a hundred lines above the rules that build and stage the pinned copy. That
  is the wording that keeps "just use the TV's FFmpeg for https" coming back, when what we ship is
  configured `--disable-network` and cannot open a URL at all.
- Root `CLAUDE.md` claimed the dual-FFmpeg-header ABI gate (n3.3 + n4.0, "two ABI tables").
  **Fixed:** one header tree, one table, and the vendored trees are gone — `vendor/` holds nanosvg
  and nothing else, so anyone who went looking found nothing and had no way to tell which half of
  the sentence was wrong.
- The host suite is **424** tests as of 2026-08-14 — it was recorded as 284, then as 396 in this
  note's own first draft, which was already stale when it was written. **Do not trust any number in
  any document, including this one.** Re-derive:
  `cd rust-modules && cargo +nightly test --lib -- --list | grep -c ': test'`.

---

## 9. What has landed (2026-08-13 → 2026-08-14)

Seven host-testable units, `make check` at **424 passed** and `make` (ARM) green throughout. **One
sentence dominates all of it: none of this has a caller in the live sign-in path.**
`auth::choose_local_connection` still filters `c.local && !c.relay` on both passes, so the 8-second
trap of §2(a) is **exactly as live in the running app as it was on 2026-08-11**, and the app still
browses one server. This is foundation plus a policy layer, not the feature — the step that connects
them is 4's second half, the race and `activate_best`.

**The data layer**

- **`plex/account.rs` — the roster DTOs widened** to `sourceTitle`, `ownerId`, `home`, `presence`,
  `publicAddressMatches`, `httpsRequired` and `Connection.IPv6`, with `includeIPv6=1` on the query.
  Every field is now null-tolerant — `sourceTitle` a real `Option`, the other strings through
  `de_str`, `connections` through `de_vec`, the flags through a new `de_bool`, the ids through
  `de_i64` — because `#[serde(default)]` covers an *absent* field and not a present `null`, and one
  strict field meeting one null fails the whole array and ends sign-in at "no server found".
  `Resource::local_connection()` (dead, zero callers) is deleted; `probe.rs` supersedes it.
- **`plex/servers.rs` — the registry**, keyed on `machineIdentifier`, replacing the `OnceLock`
  singleton whose host and port froze at the first `install`. `client()`/`client_opt()` keep their
  signatures and mean "the current server", so ~30 call sites outside `plex/` read unchanged;
  `client_for(id)`, `register`, `set_current` and `ServerId` are the additions. Why it is an
  **atomic-pointer table** rather than a lock, why every slot's `Client` is **leaked**, and why
  token generations come from a **process-global sequence** are all consequences of one fact —
  `client()` is a hot path (`posters::poster_key` calls it three times per key, per visible tile,
  per frame) — and the full account now lives where implementers will meet it, in
  **`rust-modules/src/plex/CLAUDE.md`**.
- **`plex/probe.rs` — the connection policy, pure.** No socket, no thread, no clock, so all of it is
  host-testable on Darwin. Builds and ranks candidate origins: drops a `local` connection on a
  non-owned server unless `publicAddressMatches`, suppresses every plain-http candidate when the
  owner set `httpsRequired`, and ranks local → remote → relay. It also carries, as doc, the two rules
  a real prober must honour and this module cannot: verify `machineIdentifier` on the response before
  accepting a connection, and treat `401` as its own state rather than "unreachable".
- **The relay policy — `plex::link_policy`, plus `Client::set_link`/`link()` to carry the fact.**
  A relay is a ~2 Mbit/s tunnel, so a plan that ships the file's own bytes over one stalls mid-film
  with nothing on any surface saying why. The policy denies **direct play and the container remux**,
  leaving the re-encode — the only flavor whose query lets the server pick a rate. Denying the
  remux is the half that is easy to miss: it copies the codecs and deliberately sends no cap, so it
  is the same 31 Mbit/s one layer down. It is a **branch, never a parameter** — `TranscodeSpec` has
  no bitrate field, a cap is meaningless on direct play, and the server is the only party that knows
  the tunnel's real ceiling. `route::build_stream` consults it beside the codec gates.
  **Unverified against a real relay, and not verifiable from this account** — see §7 question 5;
  what is asserted is the shape, not the 2 Mbps.

- **The sign-in chooser keeps to IPv4** (`auth::choose_local_connection`, extracted from
  `discover_and_store` so the rule is gradeable at all). This one is a REGRESSION THIS WORK CAUSED
  and did not notice for three commits: adding `includeIPv6=1` to the roster query means plex.tv now
  offers v6 connections, and the live chooser takes the FIRST `local` match and **persists** it —
  so a server listing its LAN v6 ahead of its v4 would have signed in "successfully" to an empty
  Home and written that address into the session file, making every later boot start there too.
  `stream.rs::http_open` parses a dotted quad by hand (`AF_INET`, no DNS), so a v6 literal is not
  slower, it is undialable. Found by review, not by the suite; the suite can see it now.

**The screens and the harness**

- **A shelf heading can name its source** (deliverable C): `Recently Added in Film Club · friend`,
  a second run on the same painter so the annotation cannot detach from the title under the shelf's
  lift or snap fade, and **absent rather than empty** when there is no source — no gap, no dot, no
  draw call, which is what makes it free for the single-server install. `HubRow.source` is `""` at
  both construction sites, so nothing new is reachable until step 8 populates it. One visible change
  rode along: shelf headings moved from `TEXT_PRIMARY` to the shared `TEXT_HEADING` ink.
- **A failed browse page is a STATE** (`SecFetch { Loading, Ready, Failed }`), found while mapping
  this work but a bug on the user's OWN server: a failed first page armed the retry cooldown and
  nothing else, so `total` stayed `-1`, `loading_initial()` was `total < 0`, and the grid spun for
  the rest of the session with nothing on screen admitting it. An EMPTY answer is `Ready`, never
  `Failed`. A failed *sections* list still spins, one layer up in `ensure_sections`, and belongs
  with deliverable D.
- **The harness can hand the TV a second server** — `/tmp/plxnative-servers`, a JSON array of
  additional servers beside the unchanged token file, deliberately **not** DIAG-exempt (it must
  suppress the who's-watching picker like the token file, or a headless run grades the wrong
  screen). Its own connection ranking does **not** copy the app's sign-in rule, for the reason §2(a)
  gives: public wins for a non-owned server, dotted quads beat hostnames, relay last.

Three tests are worth knowing about, because each was written after a mutation showed the suite
could not see the bug:

- The owned-server fixture carries **`publicAddressMatches: false`** — the value the live capture
  actually returns. With `true` there, deleting `&& !res.owned` from the drop rule passed the entire
  suite while, in the field, discarding **our own** `192.168.x.x` and offline play with it.
- An explicit `null` on every string and on `connections` itself is asserted to cost that field and
  never the roster — the second server in that fixture is a good one, and the test is really about
  it still arriving.
- The relay policy is graded from **both ends**: the pure answer per tier, and a server whose only
  advertised address is a relay, run through `probe::candidates` so that the ranking and the policy
  are pinned to the same `Location` vocabulary. An unknown link is asserted to restrict **nothing** —
  that is the case every play takes today, so a wrong answer there would be wrong for everybody.

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

**Next.** The line that stood here said step 2 (`ServerId` through the stored structs) was the next
mechanical diff, and that "the registry holds one server because nobody registers a second one".
Both halves are now out of date: step 2 landed, so an item is a `(ServerId, key)` pair everywhere
one is stored, and `install_pms` registers every granted server at boot, so the registry routinely
holds more than one. What remains is the probe RACE and `activate_best` (step 4's second half) —
addresses are ranked and identity-verified today, but still dialled in sequence rather than
concurrently.

## 11. Watch state follows the TITLE (2026-08-21)

**The one place the app deliberately breaks per-server semantics.** Everything else in this document
is built on view state being per-server, and it still is on the wire: two copies of one film on two
servers are two items with two `viewCount`s and two `viewOffset`s, and §1's identity rule (every
item-shaped integer is server-local and dense from 1) is exactly why they cannot be conflated. But
"I have watched this" is a claim about a **title**, not about a file on a host — so a Mark as
Watched now **fans out** to every registered source that holds the same item.

It lives in `rust-modules/src/viewstate.rs`, which was already the single owner of view-state
writes, and it reuses the resolve "Also available" was built on (`Client::find_by_guid`, one query
per source, off the SDL thread).

- **The identity is the `guid` (`plex://movie/…`), never the `ratingKey`.** §1 is not academic here:
  both servers in this household hold a `ratingKey` 4, so a fan-out matched on the key marks a
  different film watched on the other machine — confidently, and with a 200 back.
- **No resume position is ever COPIED between servers. Only the watched flag travels.** This is the
  subtle half, and it is the half `ui/alt_sources.rs` reasons out: an offset is about a file you are
  streaming from one host, which is also why that panel NAVIGATES to the other copy rather than
  swapping it under you. `unscrobble` is fanned out too — it is the other end of one control, and
  clearing the claim is still a claim about the title — but no `viewOffset` is read or pushed
  anywhere. **The consequence that phrasing hides, stated plainly:** `/:/unscrobble` clears
  `viewCount` *and* `viewOffset`, so marking a title unwatched DISCARDS the other copies' resume
  points as well — exactly what it does to the copy you pressed. Watched and unwatched are therefore
  not symmetric in cost: one adds a fact, the other throws two away, on every source holding the
  title.
- **Remove from Continue Watching does NOT fan out.** The deck is a per-server surface; taking a
  friend's item off *your* deck is not what that row promises.
- **Resolved at WRITE time, on the worker.** Not off a detail page's earlier cross-source resolve:
  the press can come from a Home / Library / Search card menu where none has ever run. A press that
  carries a guid (the detail hero, which holds the guid OF the item it is mounted on) uses it; every
  other press has one looked up from its own `(server, ratingKey)`, which costs one extra GET on a
  thread that is not drawing anything.
- **Best-effort per source, unconditional, and never fatal.** One asleep share costs one log line and
  cannot fail or retry the write the user actually pressed. There is no setting — this app has no
  preferences screen by design. A **one-source install pays nothing**: the source count is checked
  before the guid lookup, so there is no query and no log line.
- **The other copies flip on screen a round trip later, not on the press frame.** The optimistic edit
  can only reach the `(sid, rk)` in hand, because the other keys are what the resolve *discovers*;
  the fan-out reports them and the landing applies the same local edit to each, with the hub refetch
  that already follows every write reconciling the rest.

**Not verified on device.** The host suite grades the identity rule (which copies a fan-out writes,
that the pressed copy is not written twice, that a deck removal does not propagate) and the landing's
local edit; the two-server round trip itself needs a television and both servers awake.

## 12. The Home selection, per PROFILE — deliverable F (2026-08-21)

The canvas's first-run route is built (`ui/onboard.rs`), and building it settled the question the
canvas could not, because it was drawn before the owner ruled on it:

> *"Servers are configured on a PC or phone. On the television we only CHOOSE from the available
> servers. And it is separate for each profile."*

Three consequences, all of which the code now states:

**There is no add-a-server-by-address on the television, and there will not be.** The list is the
plex.tv grant — the existing registry — and nothing else. (It was transport-blocked as well:
`stream.rs` takes a numeric address and speaks cleartext.)

**The selection is keyed by PROFILE.** `Session::pinned: Vec<PinnedLib>` hung off the `Session`,
which is one per install — so a household could hold exactly one opinion about a friend's films,
and switching profile left the previous person's shelves on the front door. It is now
`Session::home_pins: Vec<HomePins>`, keyed by the Plex Home user's **`uuid`** (empty = the account
owner with no Home selection), which is the same shape and the same reasoning `recent_searches`
beside it already carries. A switch needs no code of its own to honour it: `install_pms` calls
`browse::reset`, discovery re-runs, and the re-resolve reads the new profile's record.

**`HomePins` records BOTH sides of the answer** — `on` and `off`, not one list of pins. A single
list cannot tell *turned off* from *not answered about*, and libraries arrive over time: a share
whose server was slow, a library the owner created last week. One that lands after the question was
put must fall on its own DEFAULT, not silently Off because it was absent from a list written before
it existed. That is also what makes the canvas's "a share arriving later does not reopen this
screen" honest rather than merely quiet.

The rules are `plex::pins` — pure, no store, host-graded: the ownership default, the recorded
answer, the "more than one source, once per profile" gate, and a **never-empty floor** (a recorded
selection CAN be emptied without any toggle — pin only a friend's library, then lose the friend
from the roster). `browse.rs` is the plumbing around them, and `toggle_pin` now persists: every
flip was in-memory before, so a selection made in the Sources panel was gone by the next boot and
the ownership default came back, which reads as the switch not working.

### Where the implementation reinterprets the canvas

Four places, all recorded so the next reader does not "fix" them back:

1. **Per profile.** The canvas predates the ruling and says nothing about profiles. The owner wins.
2. **Your own server's group header carries no accessory.** The canvas gives it `"This account"`;
   the shipped Sources panel draws nothing, on its own stated rule that an empty handle is *the
   absence of an owner rather than an anonymous one*. The route and the panel must agree, and they
   do — that rule is the one that stays.
3. **A borrowed-only account gets every library On.** The canvas's "a friend's arrives Off" has no
   answer for an account with no server of its own; taken literally it opens the app on nothing.
   With nothing of your own to prefer, a borrowed library is simply a library.
4. **Focus opens on *Start watching*.** The canvas says so twice in prose and draws it that way —
   but a stale comment in its own `renderVals` claims focus opens on row index 2, "the first shared
   library". The prose and the artwork agree with each other, so the comment is the odd one out.

### The screen, and how to look at it

`ui/onboard.rs` mounts `ui::source_list` — the SAME row-model builder the Library toolbar's Sources
panel uses, extracted out of `ui/library.rs` for exactly this reason. It differs by two arguments,
not by a second builder: every library rather than the browsed type's (`browse::all_source_rows` —
there is no tab bar here to be scoped to), and no *Check for new shares* tail.

**`/tmp/plxnative-firstrun` forces the route.** A screen asked once per profile is otherwise
unreachable the moment you have answered it, and the two-source roster it needs comes from
`/tmp/plxnative-servers`, which marks the boot automated — and an automated boot is exempt from the
question, exactly as it is from the who's-watching picker. Both halves are why looking at this
screen headlessly needs a trigger of its own.

One fix fell out of building it and applies to the Sources PANEL too: the counts and the machine
names land without changing the section table's SHAPE, and both surfaces were keyed on
`sections_gen()` — so rows read "Films" long after "26 films" had arrived and an unnamed group drew
no header at all. Both now watch `browse::source_list_gen()`, the shape plus the facts the rows
state.

### How the answer actually reaches Home — the join the section table cannot make

Found in review, and it is the failure that would have made the whole screen look ornamental: **the
answer has to govern Home on a boot where the friend's server is never enumerated at all.**

`pms::feeds_home`'s standing rule is that *a library nobody has discovered is undecided, not
unpinned* — §6's own bootstrap, and correct, because the pin is a decision about libraries and you
cannot have decided against one that has never appeared. That was harmless while every granted
library defaulted On. It stops being harmless the moment a friend's library defaults **Off**,
because Home is the one screen that never enumerates: boot fetches the CURRENT server's sections and
no others (`browse::ensure_sections`, deliberately — fanning out over the roster parks the SDL loop
on one `connect(2)` per sleeping share), and the discovery pump runs from the Library, Search and
first-run screens. So on the second and every later boot the share sat in the roster with **no row
in the section table**, "undecided" applied, and a friend's shelves went back on the front door of
somebody who had turned them off the night before — including the person who simply pressed *Start
watching* on the defaults.

Two halves close it, both in `browse.rs`, and both keyed on the machine id because that is the only
name a record has:

- **`RECORDED`** — the current profile's persisted answer, kept in memory from the read
  `resolve_pins` already makes, and joined into `library_pins()` for every roster source the section
  table does NOT hold. The record is on disk keyed by machine; that is exactly the join that was
  missing. It is a snapshot rather than a per-call read because `library_pins` is reached from
  Home's own pump, and `session.rs` forbids a per-frame file read.
- **`pins::carry_forward`** — a record is written from the section table, and `set_pins_for`
  replaces a profile's entry wholesale, so one switch flipped on a boot the share missed would
  otherwise have erased the share's answer and let the ownership default back in. The merge grain is
  the MACHINE: a server the table holds has just been answered about in full (a library it has since
  lost is correctly dropped); one it does not hold has not been answered about at all.

Both are graded on the host, with the negative case checked — `browse`'s
`a_recorded_answer_reaches_home_before_that_servers_sections_do` and
`a_flip_made_while_a_share_is_absent_does_not_erase_its_answer` both fail if either half is removed.

A third, smaller ordering bug went with them: the stored-session boot called `install_pms` — whose
section fetch resolves the Home selection — **before** `session::set_current`, so that resolve ran
against the owner's record whoever was signed in. The switch path (`auth::take_ready`) already had
the two the right way round; the boot path now matches it.
