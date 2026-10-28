# plex/ — the Plex data layer (typed PMS + plex.tv client)

This is the **typed Plex client**: account/login (`account.rs`, `session.rs`), the HTTP client
(`client.rs` — one server) and the registry that holds them (`servers.rs` — WHICH servers exist and
which is current, with `probe.rs` deciding which of a server's addresses is worth dialling),
library/hubs/metadata reads (`library.rs`/`hubs.rs`/`models.rs`), and the whole
playback protocol — the MDE/transcode decision + capability profile (`transcoder.rs`), the
timeline/PlayQueue/identity session ops (`timeline.rs`), stream selection + the direct-play
target (`library.rs`), with typed request params in `params.rs`. **Every PMS query in the app
is built here** (the `route/` module holds playback *state* (`decision.rs`) + policy (`plan.rs`),
never a query string). The
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
credential-eligible server** now — which is why nothing outside `plex/` changed when the
`OnceLock<Client>` singleton became a table. `client_for(id)` is the multi-server addition;
`register_origin(machine_id, &Origin, token, Option<&ResolvePin>, ConnectionFacts)` puts a server
in the table. **`install(&Origin, token, Option<&ResolvePin>, ConnectionFacts)` is the SESSION
path** (boot, QR login, profile switch) and retargets when the origin may carry a credential — it
grew the fourth parameter in #95 step 8: `ConnectionFacts{tier, ip}` is
applied to the published `Client` INSIDE the same registration write that creates or re-points its
slot, never as a separate post-hoc `set_link`/`set_connection` call a caller could forget or a
re-point could race. `None` in either field means **leave unchanged**, not "set unknown" — a
same-origin retoken that knows nothing new about the connection passes `ConnectionFacts::default()`
and the client's prior tier/IP survive; only a re-point (a genuinely fresh `Client`, which
`Client::new` starts at `LINK_UNKNOWN`/`IP_UNKNOWN` regardless) or an explicit `Some` actually
changes what's stored. `register_origin` takes `ConnectionFacts` directly rather than keeping a
connection-less twin beside it; `register_captured_origin_with_connection` is the `pub(crate)` seam
that carries it through the other registration paths (dev boot, `install_captured_registry`'s
primary and extras, the endpoint/roster/candidate-activation handlers in `auth.rs`); every one of
them derives the IP family from the candidate's own advertised `address`, never `Origin::host()` —
a `plex.direct` origin's host is a certificate NAME `IpVersion::of_host` cannot parse as a literal,
which is why that used to read `unknown` on almost every real boot (issue #95's R3(a)).

**A server's address is an `Origin` — scheme + host + port — and it is PARSED FROM A URL, never
assembled from an address.** `net/origin.rs` is the type and the reasoning (it lives in the transport so the
transport can read it; `plex/origin.rs` re-exports it and keeps `CredentialPolicy`); the short version is that
plex.tv advertises a server's TLS origin as the `plex.direct` HOSTNAME (`Connection.uri`) while
`Connection.address` stays the dotted quad behind it, and the certificate is issued for the name —
so a control plane carrying only an address can never validate one, however much TLS is added
underneath it. `probe::Candidate::origin()` is the one derivation, and `Candidate::address` survives
only as what a log and the Sources panel SAY. Two invariants come with it: `Origin::host()` is
**always unbracketed** (it is the `getaddrinfo` node) while `Origin::authority()` **always**
brackets a v6 literal (it is URL serialization) — confusing the two makes URL construction keep
looking right while name resolution quietly stops working. `register(machine_id, host, port, token)`
and `Origin::http(host, port)` still exist and are honest about what they mean: **every caller of
either is a place that still assumes cleartext**, which is what makes them the grep for the TLS
work. The CONTROL plane no longer makes that assumption: `http.rs` sends an HTTPS origin through
libcurl. Neither does playback: `StreamUrl` preserves the scheme and `ff.rs` selects `stream.rs`
for plaintext or `curlio.rs` for HTTPS. One dev-only caller also still throws an origin away:
`metadata.rs`'s `alt_stand_in_slot` registers a stand-in from `c.host()`/`c.port()` and must move
to `register_origin`. (It was `ui/alt_sources.rs`'s until restructure phase 10 moved the *Also
available* store to the data layer beside the resolve that fills it.)

**A `plex.direct` origin is dialled at the address plex.tv advertised beside it, with no DNS.**
`origin::ResolvePin` (2026-09-05) is the offline-mode fix: the persisted origin for the household's
own server is normally `https://192-168-0-10.<hash>.plex.direct:32400`, a name only Plex's public
zone resolves, so a LAN whose uplink was down could not reach a server one hop away — and the
plaintext twin `probe::candidates` documents as "the offline fallback" cannot carry a token in a
store build (`http::credential_transport_allowed`; a consented `grant` needs a fresh plex.tv
resource list, which an offline boot does not have). A pin is built ONLY when the dashed label
encodes the stored `address` (v4 or the eight-group v6 spelling), so it is a pure function of the
hostname; `register_origin`/`install` take it, the `Client` carries it for the control plane, and
`net::resolve` holds an append-only table the media plane (`curlio`) consults by host and port.
TLS validation is untouched here: the name stays in the URL and in SNI. (The only relaxation of CA
verification on the ordinary request path, issue #378's key mode below, is a separate and narrower rule about a wrong clock; a pin
changes where an address comes from and nothing about what is trusted.) `/tmp/plxnative-nowan` makes
every unpinned name fail as a dead resolver would, which is how the case is reproduced on a desk.
**Since issue #95 the DISCOVERY PROBE is pinned too, not only the winning `Client`.**
`auth::race_batch` builds a `ResolvePin` for each `https://…plex.direct` candidate from the same
`Candidate::address` this module already carries, and hands it down through `auth::get_identity` to
`http::request_probe`'s (or `request_probe_learning_key`'s) TLS arm — the identical mechanism `register_origin`/`install` use, run one
step earlier, at the DIAL that decides a winner rather than only after one is already decided. That
is what turns a router's DNS-rebind protection (which answers every `*.plex.direct` name with
NXDOMAIN, so the probe's own resolver never reaches the LAN candidate) from a permanent relay
detour into an ordinary pinned LAN HTTPS winner; the plaintext twin's own probe is unaffected — a
pin belongs to a TLS name, never to a literal — and a candidate whose dashed label does not encode
its `address` simply gets no pin and resolves through DNS exactly as before.

**The probe also learns each server's leaf public key (issue #380); issue #378's key mode reads it.** While
online, over a connection libcurl verified in full (`Tls::Ca`, never the lab's pinned mode), an
`/identity` answer that `auth::classify` accepts for the machine asked for makes
`ProbeReply::grade_learning` record that machine's pin — `sha256//<base64>`, the exact
`CURLOPT_PINNEDPUBLICKEY` string `spki::pin_from_pem` makes of the chain's index-0 certificate, read
through `CURLINFO_CERTINFO` (`net::peer_leaf_pin`) only when the request opts in
(`http::request_probe_learning_key`; the option makes libcurl decode the whole chain, so no ordinary
request pays). **Only an origin that has a `ResolvePin` opts in** — `auth::get_identity` picks
`request_probe_learning_key` when it holds a pin and plain `http::request_probe` otherwise, the one
place the rule is spelled. The dashed `*.plex.direct` names are the only origins the offline fallback
can ever apply to, and a custom server-access URL behind a proxy with its own certificate would
otherwise rewrite the machine's single entry on every discovery. Plaintext, a failed verification,
a mismatched `machineIdentifier`, an origin without a pin and a relay route (skipped as a
conservative choice) learn nothing. It lives in `Preferences::server_key_pins`, one entry per
`machineIdentifier`, session-level like `plaintext_consent` (NOT in `ServerRef`/`SourceRef`/
`ProfileCreds`, which are cloned per profile), soft-parsed and skipped while empty.
**It is in the PUBLIC preferences, so sign-out forgets it by an explicit rule, not with the
credentials:** `Mutation::ClearTenure` retains `public.preferences` whole except the keys in
`storage::state::ACCOUNT_BOUND_PREFERENCES` (the account's own lists, `server_key_pins` and `plaintext_consent`, issue #385, and every profile's remembered browsing and playback memory: `track_memory`, `subtitle_offsets`, `library_sorts` and `last_library`; read the constant for the current set), so sign-out — and "Delete all
local data", which runs it first — leaves the stored record with no learned key, and the next account to sign in opens with none
(`migration_tests::helper_signout_forgets_the_learned_server_keys_and_the_next_account_inherits_none`;
for the answers, `helper_signout_forgets_the_plaintext_answers_and_signing_back_in_asks_again`).
`session::learn_server_key` queues the write only when the pin changed.
`Session::server_key_pin(machine_id)` is the accessor.

**Key mode (issue #378): the remembered key stands in for the chain-and-date check.** The television
has no battery clock, so a valid `*.plex.direct` certificate can read as expired (or not yet valid)
before NTP has run, and the strict handshake then fails with rc 60 and `CURLINFO_SSL_VERIFYRESULT`
10 (expired) or 9 (not yet valid). When that is the failure libcurl reported, and `net::keypin` holds
a remembered key for that exact `host:port` — after a strict attempt, or after a roots-mode attempt
(below) that verified the chain and then failed on the date, the shape of an old trust store behind a
wrong clock — the request is repeated once with `CURLOPT_PINNEDPUBLICKEY` set to it, `CURLOPT_SSL_VERIFYPEER` 0 and `CURLOPT_SSL_VERIFYHOST` still 2.
What is relaxed is the chain-and-date check; what still holds is that the leaf's name matches the
host dialled and that its public key hashes to the remembered one (a different key is rc 90, never
served). **The security of key mode rests on the key pin plus the name check, and on nothing the date
check used to imply.** Three facts follow, and none of them is hidden: (1) verify result 9/10 does
NOT prove the date was the only problem, because OpenSSL stops at the first error it meets walking
the chain, so an untrusted issuer behind an expired leaf also reads as 10; (2) key mode engages for a
genuinely expired certificate on a CORRECT clock too, since the trigger is the verify result and
never a judgement about the clock; (3) the remembered key has no expiry of its own, it lasts as long
as the session that holds it. A different verify result (untrusted issuer reported first, wrong name,
revoked, a valid leaf) never enters key mode, and neither does plex.tv, a host with no remembered key,
or a plaintext URL. `net::keypin::apply` sets the pin FIRST and checks it: `VERIFYPEER` goes to 0
only after libcurl accepted the pin, so a libcurl that refuses the option leaves the request strict
and failed, and `confirm` then compares the presented leaf's key itself (via `CERTINFO`) in case a TLS
backend accepts the option and never enforces it. Every key-mode handle also sets
`CURLOPT_FRESH_CONNECT` and `CURLOPT_FORBID_REUSE`, so it always does its own handshake (a reused
keep-alive connection has no certificate to confirm) at the price of one handshake per key-mode
request. A key-mode answer is never learned from (`peer_pin` is never read), so key mode cannot
rewrite what it is checking against.
**One table, filled once:** `net::keypin` is process-wide, keyed by lowercase `host:port`, with
replace semantics. `session::replace_cache` is the single choke point that projects the session
into it (`session::project_server_keys`: each machine's key bound to the hosts of the stored
`session.server` and `session.sources` that carry a `ResolvePin`); `servers::register_lazy` binds a
machine registered at runtime; a key `session::learn_server_key` learns reaches the table only when its
queued write is applied and projected (the projection is the only production source of a key; `register_lazy` only binds hosts);
sign-out empties it (revoked/missing/cleared sessions project nothing). Both stacks share
the decision and the option code: the control plane in `net::request_tls_sink` (wrapped by `request_tls_evidence`), the media plane
in `curlio::CurlSource::start_range_until`, which every open, reopen and seek goes through. After a
key-mode success the host is LATCHED for 10 minutes on the monotonic clock (later requests skip the
doomed strict handshake; the timer does not slide; a strict success or a changed pin clears it; rc 90
in key mode clears it and is the failure). The log carries one line when a host first engages, with
the year the device believes it is, and one for rc 90, and never a pin or a host:port. The same
decisions also publish facts the app polls (`keypin::engaged`, `blocked_for(machine)`, `revision`;
per host and asked per machine; a host's fact is its latest strict outcome, cleared by its success,
a pin change or, for "no key", a later strict failure that is not about the date), and the first engagement of an app run raises ONE
television toast (`app::clock_notice`; never retried, host-tested only until the set accepts it).

**Roots mode: the bundled public roots stand in for a CA store that predates the issuer.** The other
way a `*.plex.direct` handshake fails on an old television is not the clock but the issuer: Let's
Encrypt moved to new roots in 2025 (ISRG Root YR, YE) and a 2020 firmware's store lacks them, so
the strict handshake fails with rc 60 and verify result 20 (or 2, 21) at depth 1. When that is the
failure, and the request's host is a `*.plex.direct` NAME (`keypin::is_plex_direct`: a suffix match
on the URL's host with only DNS-name characters, so `plex.direct.evil.example`, `notplex.direct` and
a `@`-smuggled authority do not count, and `keypin::key_of_url`, the one reading of a URL both planes
use for this, gives an authority that is not a plain `host[:port]` no key at all) and the shipped bundle
`le-roots.pem` is in the app directory
(`paths::in_app_dir`; root-owned and read-only on the set; a non-empty readable file), the request is repeated once on a
fresh handle with `CURLOPT_CAINFO` set to it. **Nothing is relaxed**: `VERIFYPEER` 1 and `VERIFYHOST`
2 are stated again by `keypin::apply_roots`, so the chain, the dates and the name are all still checked,
against four public roots (X1, X2, YR, YE) and nothing else beyond what the firmware's libcurl reads
by default from a CA directory. A libcurl that refuses `CURLOPT_CAINFO` sends nothing and the
request reports the strict failure. A missing, empty or unreadable bundle means the fallback never engages (one log line).
**One decision, `keypin::after_failure`, both planes call it and nothing else decides a rung.** After a
strict failure: a date result (9/10) goes to key mode when a key is held; a missing issuer (2/20/21) on a
`*.plex.direct` name goes to roots mode, once. After a roots-mode failure: a DATE result goes to key mode
when a key is held (strict failed on the issuer, the bundle then verified the chain and refused the
dates, and the key learned on an earlier good boot is all that is left; with no key held the attempt
publishes `Blocked::NoKey` exactly as a strict date failure does, `keypin::roots_unanswered`); and a
roots attempt that STARTED the request from the latch, refused for its verification (60/51), goes
strict once, because the certificate may have moved to an issuer the device store now trusts but the
bundle lacks. A roots attempt a strict failure led to has nowhere else to go, the bundle is never
offered twice in one request, and a date failure never goes back to strict, so a request is at most
three attempts. A name mismatch, a self-signed leaf (18/19), plex.tv or any other host gets neither
mode. After a success the host is latched in the same `keypin` state for 10 minutes on the monotonic
clock (later requests skip the doomed strict handshake, the timer does not slide, a strict success or
a refusal by the bundle clears it); both stacks share the decision, the control plane in
`net::request_tls_sink` (wrapped by `request_tls_evidence`) and the media plane in `curlio::CurlSource::start_range_until`. Unlike key
mode a roots-mode answer IS strictly verified, so it may teach `peer_pin`. The log carries one line when
the fallback engages, one when it first succeeds and one when the bundle refuses too, naming only the
verify result.
**Redirects (media plane).** `curlio` follows redirects itself and libcurl does not
(`CURLOPT_FOLLOWLOCATION` is 0 on every handle): `start_range_until` loops `run_hop`, one easy handle per
hop, and `next_hop` decides each one. It counts against `stream::redirect::MAX_HOPS` (more is
`Transport(47)`), refuses a redirect out of a TLS hop to plaintext, and uses the `Location` exactly as
spelled, so the original URL's query, where the token is, is never copied to a hop. `Hop::of` takes the
`keypin` key and the `net::resolve` entry from the hop's OWN host, so a hop starts where `keypin::begin`
puts that host: strict against the device store, on the bundle only if the hop is itself a `*.plex.direct`
name whose issuer the store lacks. A hop out of a roots-mode host is therefore verified against the
television's store, and a hop whose issuer only the bundle holds is refused. A hop's failure is its own: its
60 does not latch the redirecting server into roots mode, and its key is never published as that server's
`Blocked::KeyChanged`. A seek or reopen replays the chain from the URL the source was opened on; the
landing URL is the private `CurlSource::hop_url`, and no public method returns it. `stream_redirect`, the
plaintext follower, never touches `keypin`, stops at the first https hop and hands that target to `curlio`,
where the hop count starts fresh. It also differs on credentials: it re-appends the original token pair to a
same-origin hop, and `curlio` never copies the query. Pinned by
`curlio_roots_tests::a_redirect_from_a_roots_mode_host_is_verified_against_the_device_store`,
`curlio_roots_tests::a_redirect_hop_is_not_verified_against_the_bundle_the_open_before_it_was_served_through`,
`curlio_roots_tests::a_redirect_hop_that_fails_verification_does_not_send_its_server_into_roots_mode`,
`curlio_keymode_tests::a_redirect_from_a_key_mode_server_to_a_host_with_another_key_does_not_blame_the_server`
and the contract tests in `curlio_redirect_tests.rs`.

**The who's-watching pick is seated from `Session::profiles` when plex.tv does not answer.** The
first real outage (2026-09-06, `docs/measurements/offline-picker-red-tv-2026-09-06.log`) got past
the pinned origin and then could seat nobody: every pick is a `POST /api/v2/home/users/{uuid}/switch`,
and the one no-network shortcut (re-picking the active, PIN-free profile) did not cover a house
whose active profile is the PIN-protected admin. So every ONLINE seating now writes a
`ProfileCreds` record — user, primary, roster, and for a protected profile a `PinVerifier`
(PBKDF2-HMAC-SHA-256 under a random salt, `plx_base::sha256`; never the PIN) — and
`auth::switch_thread` reads `account::SwitchOutcome`: plex.tv's verdict (`Refused`) ends the
switch as before, `Unreachable` falls through to `auth::offline_activation`, which seats a cached
unprotected profile on the pick and a cached protected one on its PIN, and a profile this set has
never seated online says so: "No internet connection. Pick this profile once while online, and it
will work offline." (The QR screen carries the same sentence about signing in.) `account::plex_tv_recently_unreachable` (a 45 s
memo the picker's own roster refresh usually fills) sends the pick to the cache FIRST so an
outage does not cost a connect timeout per pick. Three simulator runs in `docs/measurements/
offline-picker-sim-*-2026-09-06.log`, and `tests/run.py`'s `offline_pick_cached` on the set.

**A candidate only becomes the LIVE origin if this build can put a token on it.** The probe race
used to activate "the first usable answer immediately", and on a LAN the plaintext twin answers
before the TLS handshake completes — so a store build re-pointed its live server to an origin it
then refused (`security: refused plaintext PMS credentials`) for the ~100 ms until the https winner
landed, and whatever was in flight (a hub fetch, the picker's first avatar) failed for good. Issue
#95's fix is that eligibility is decided once, at synthesis, not asked again at activation:
`probe::candidates` stamps each `Candidate::credential_eligible` from the `CredentialPolicy` the
plan was built with, and only an eligible answer can become `first`/`best`/get activated — a
verified plaintext answer in a store build does **not** count as reached and does not hold back the
relay leg. If nothing eligible verifies, the result is `Reach::InsecureOnly` (plan §4's precedence:
`At` > `InsecureOnly` > `Refused` > `TlsUntrusted` > `No`), which becomes `Outcome::InsecureOnly` /
`Discovery::InsecureOnly` / `SourceState::InsecureOnly` ("Not secure") — a fifth sentence, told
apart from `Unreachable`, that **outranks a 401**. Before this it counted as reached, which was
issue #95 itself.

**Why a server could not be used is `probe::Cause`, beside the `Outcome`, never more variants of
it.** `Outcome::Unreachable` still means "no address verified" and the sources panel and registry
read it so; underneath it the probe tells `Cause::Unreachable` (nothing answered) from
`Cause::TlsUntrusted { verify }` (HTTPS reached something whose chain this television's trust store —
and, for a `*.plex.direct` name, the bundled roots — could not verify: libcurl 60 with an X509 verify
result of 2, 18, 19, 20 or 21, `RequestFailure::untrusted_chain`; **never 9 or 10, which are the wrong
clock**) and `Cause::Unauthorized` (a 401). `Reach::TlsUntrusted` sits between `Refused` and `No`. The
cause travels on `SettledProbe::cause()` per `machine_id` and is what the failure read-outs are worded
by: `browse.auth.no_source_access` ("has no access to this server") is for a 401 and nothing else — a
profile switch where no server verified says `tls_untrusted`, `no_source_access` or
`servers_unreachable` by that order (`FailureCauses::worst`). A switch or discovery in which NO server
verified at all writes one closed `auth: no server verified — unreachable=N tls_untrusted=N
unauthorized=N verify=[…]` line (counts and verify codes only); a plaintext-only verdict, an
admission refusal, a malformed reply, a no-servers account and a plex.tv failure keep their own
existing lines instead.

**"May a credential go to this origin" has ONE answer: `grant::credential_allowed`** (or
`grant::allowed_under` where a pure function receives the policy). It is the build's
`CredentialPolicy` OR a live `PlaintextGrant` for that exact origin (PLX-NATIVE-10). Whoever puts a
particular SERVER's token on an origin — registry and endpoint admission — asks
`grant::allowed_for(policy, machine_id, origin)`: a grant admits only the machine it was minted
for. A remembered (cached) origin asks `grant::remembered_allowed`: the policy alone, never a
grant. Nothing else
asks `CredentialPolicy::may_carry_credential` or `Origin::is_tls` for a credential decision —
`grep -rn may_carry_credential . --include=*.rs` (from `rust-modules/`) finds only `grant.rs` (plus doc links). `probe::candidates`
stamps the policy half at synthesis, and `auth::settle_plaintext` adds the grant half after the
race. A grant is minted only by discovery, from a FRESH verdict `InsecureEvidence::
plaintext_eligibility` calls eligible, when the person's recorded answer (`Session::
plaintext_consent`, captured at the spawn site as `grant::PlaintextAsk`) allows it; it is bound to
{identity generation, network generation, machine, exact numeric origin}, never persisted, dies on
sign-in/sign-out, on every DID foreground (which queues each stranded server's endpoint
re-discovery, requested by `grant::UpgradeRetry::due`), on a refusal (which also moves the consent generation
a `PlaintextAsk` captured) and on a roster commit that does not install its (machine, origin) —
a roster commit moves no generation — and a stored `SourceRef` naming a
plaintext origin registers tokenless until discovery re-mints. Ending a grant re-grades the
registry (`servers::regrade_credentials` blanks every client a grant was carrying, and `mark_on_grant` sets the per-slot `on_grant` flag);
`grant::UpgradeRetry` re-discovers a granted server on the hub-retry backoff and the HTTPS
registration retires the grant (`auth::retire_grant_on_https`).

**Online primary selection also proves the token after it proves the machine.** `/identity` is
deliberately unauthenticated, so a fresh identity winner is only a known endpoint. Before sign-in,
rediscovery or a profile switch may select it as primary, auth sends `GET /library/sections`
through that source's exact origin, resolve pin, transport policy and per-machine token. A valid
empty sections container is success; 401/403, timeout, transport refusal and malformed JSON remain
distinct evidence, and primary selection continues with the next eligible endpoint/server.
Secondary servers keep the profile-specific grants plex.tv returned live and cached; their identity
probes refresh endpoint and reachability facts without making every secondary browse before it can
be registered. Direct identity candidates settle as one race, and a relay fallback receives its own
local/remote probe opportunity. **The race does not wait for a route that cannot change the
answer** (`auth::race_decided`): once a verified, credential-eligible winner exists, a candidate
that cannot outrank it (`candidate_score` is a function of the candidate alone) is left behind, and
a better-ranked one that has not answered gets `ProbeDeadlines::grace` (1.5 s) from the first
win, not its own 5 s/10 s deadline. Waiting for every candidate made a healthy sign-in cost a
dead public address's full deadline, so "taking longer than usual" ran on every sign-in
(`screens/login.rs`'s `DISCOVERY_ESCAPE_AFTER_MS` derives its number from this cost). A race with
no winner still runs every candidate to its deadline, and the relay phase still starts only then. Authenticated fallback attempts share a separate 20-second budget,
with each request still capped at 5 seconds for Local and 10 seconds for Remote/Relay. Only time
inside those authenticated requests is deducted: identity probing (including later direct/cached
or relay attempts) and inter-server pacing do not spend admission time. Offline cached-profile
seating keeps its existing PIN/cache contract.

Slots are keyed on `machineIdentifier` because that is the only identity that survives a server
changing address — and a registration that has *learned* an id **adopts** an address-only slot
instead of adding a second one for the same machine.

Three design choices carry the weight, and each is a prevented bug rather than a preference:

- **An atomic-pointer table, not an `RwLock`.** `client()` is a HOT path: `app::adapters::poster::built_key`
  calls it **three times per key, for every visible art tile, every frame** (~25–40 tiles × 60 fps).
  A read is one relaxed load, one acquire load, a deref — no lock, no refcount, no allocation. An
  `RwLock` would add an atomic RMW pair per call plus a fairness stall every time a login writes,
  and an `Arc` would change every call site's type to buy a refcount bump per tile per frame.
- **Each slot's `Client` is LEAKED, deliberately.** That is what makes handing out `&'static
  Client` sound without an `Arc`: the reference a worker took at frame N must still be valid when a
  re-point lands at frame N+1 with that worker mid-request. Re-pointing publishes a NEW leaked
  `Client` over the pointer, so the worst case for the old reference is **one request sent to where
  that server used to be** — never a dangling pointer, which is the failure that has no debugger on
  this device. The leak is one small struct per registration ever made, written on login / profile switch /
  server switch, never per frame; ids are never reused and the table grows by segments, with no
  server-count ceiling short of the `u16` id space. A sign-out removes the revoked slots' rows from
  the keyed per-server tables (`serverinfo`).
- **Token generations come from a process-global sequence, so no two clients ever share one.**
  `token_gen` was a single process-wide counter, which cannot express "server B's token changed".
  Its only reader is `app::adapters::poster::built_key`'s memo and that memo compares **one number** — so two
  servers whose generations happened to agree would mean that the moment `client()` started
  answering with B, the memo said nothing had changed and served B its cards from **A's memoised,
  token-bearing paths**. Uniqueness makes "did this number move" also answer "is this even the same
  server".

## Granted, pinned, reachable — three states, and never the same question

The whole shared-source feature rests on keeping these apart:

- **granted** — plex.tv's answer. `/api/v2/resources` says this account may use this server and
  hands over the `accessToken` that proves it. Not a setting of ours; it is the owner's decision.
- **pinned** — the only thing the USER controls, it governs **every browsing surface**, and it is
  **per Plex Home PROFILE**. The user reads it as **Favorite libraries**; the identifier and the
  persisted `Session::home_pins` key keep their names on purpose (renaming the key would break
  rollback, not upgrade). It governed **Home alone** until 2026-09-05 and this paragraph said so;
  the owner's direction was that the setting affects the whole app. What reads it: Home's shelves,
  the top tab STRIP (a type with no favourite library draws no pill), and the Library's Sources
  picker. What does NOT: the browse grid, sort, the A–Z rail and the item pages, which are all
  downstream of a library you already chose — and **Search**, which stays grant-scoped and only
  RANKS favourites first, because a browsing preference is not an authorization boundary and
  removing results would invent a false negative for a film the user owns and can play. The
  unscoped list survives as `browse::all_source_rows`, which is the Favorite libraries editor's, and
  is the only way a non-favourite comes back. The rules are `pins.rs` (pure); the store is keyed by
  the profile's `uuid`; the page that asks once is `AppArg::Onboard` (`Route::Onboard` before
  restructure phase 12 (D1) retired `enum Route`), first-run *Favorite
  libraries* — `ui::onboard` through phase 4, `screens::onboard`'s owned `OnboardScreen` since
  phase 5b (2026-09-07); reached again later from Settings it is a page of that family rather than
  this same route (`SettingsPage::Favourites` — the enum lives in `screens/family.rs`, not
  `screens/settings.rs` — hosting the same screen type). Owner's ruling, 2026-08-21 — "it
  is separate for each profile" — and it hung off the whole `Session` (one per install) before that.
- **reachable** — a fact about NOW: something answered at one of its addresses, *as the right
  machine*. It changes while nobody touches anything, and it is never a reason to forget the grant
  or the pin.

Collapsing any two of them produces a specific wrong behaviour rather than a vague one. A `401` is
the final answer only when no parallel direct or fallback relay candidate verifies the machine; at
that point reading it as "unreachable" sends the user to the router when the fix is to refetch
`/resources` or correct the access policy. An unreachable server read as un-granted drops the
source out of the Sources list, so there is nothing left to retry.
A pinned-but-dead source drawn as a shelf is a spinner that never ends — the design's answer is that
a dead source is **absent** from Home and states itself in its own library section instead.

## Gotchas that bite (all verified in code)

- **`notify.rs` is the PMS server-sent-event feed, and it is not a playback signal.** It parses
  `/:/eventsource/notifications?filters=playing%2Ctimeline` (`docs/pms-api.md` §9: the parameter is
  `filters`, the spec's `filter` is wrong; the encoded comma list was measured to deliver `timeline`) into
  lenient `Notification`s (`Play`, `Library` for finished movie/show timeline entries, `Ping`,
  `Ignored`; a `timeline` frame is `event: timeline` + one `{"TimelineEntry":{...}}` object, state 5 both opens
  and closes a sequence so "created" is state 5 with no `metadataState`, and state 9 for a delete is UNVERIFIED,
  never captured) over `http::stream`, dropping this
  device's own sessions at the source, and never mints a client id (`client_id` is the persisted
  one; a new id raises a new-device alert). Its policy lives in `plx_data::stores::notify`: shipped
  ON (`NOTIFY_ENABLED`), owned servers only. Nothing here may call a stop, an end-of-stream candidate or an Offset
  publish: a stranger's `stopped` session would otherwise scrobble our other servers.

- **A hub's `title` is PMS's own localized text — except for the standard hubs, which this app
  now overrides client-side at BOTH scopes it draws hubs on.** `hubs.rs`/`models.rs`'s
  `Hub::title` still carries whatever PMS sent, but neither `screens/home/mod.rs` nor a library's
  own browse grid renders it verbatim: `plex::hub_title::localized_hub_title` is the ONE shared
  table both `pms.rs::project` (Home's `/hubs` merge) and `browse::section_hubs::parse_hubs` (a
  library's own `/hubs/sections/{id}`) call, so the two cannot drift apart, parameterized by a
  `hub_title::Scope` (`Home` / `Section`) because PMS itself titles the "Recently Added" family
  differently at the two endpoints. For a hubIdentifier the catalog recognizes it substitutes a
  client-side string, **unconditionally**, the same way `home.continue`'s title never came from
  PMS at all (`i18n::msg::browse_home_continue_watching()`, set where the dedicated
  `/hubs/continueWatching` deck becomes a `HubRow`):
  - Home: `home.ondeck`/`home.onDeck` → "On Deck", `home.playlists` → "Recent Playlists"; the 5
    whole-server `home.*.recent` ids → a per-type string ("Recently Added Movies") when the id
    names the household's ONLY hub of that type in the response (`pms.rs::project` counts
    `hubIdentifier` occurrences before choosing — `hub_identifier_counts`), else (PMS minted more
    than one, or the id is a numbered `movie.recentlyadded.<id>`/`show.recentlyadded.<id>`/
    `tv.recentlyadded.<id>`) → "Recently Added in {library}" using the hub's own
    `librarySectionTitle`.
  - Section: the same `*.recentlyadded.<id>` family → plain "Recently Added", no library name —
    the section page already is that library, and PMS itself drops the qualifier at this scope
    (§3a). A per-section deck (`*.inprogress.<id>`) and every id this catalog has not specifically
    enumerated keep PMS's title verbatim at this scope too.

  `client.rs::headers`/`pms_headers` still send the literal selected UI tag
  (`identity::language()`) as `X-Plex-Language` on every PMS operation, hubs included, which the
  `pms_headers_carry_the_literal_selected_ui_language_be_included` test pins for `en`/`es`/`be` —
  that header behavior is unchanged, only what each screen does with the *response* changed. A
  Home screen with SOME hub titles translated into the selected language and others not (issue
  #12, a Belarusian UI mixing `be` and ru/en titles) was PMS's own per-string translation coverage
  for that tag answering back on every shelf; it now answers back only on a hubIdentifier this
  catalog has never enumerated — a custom collection shelf (`custom.collection.*`), a rotating
  genre/actor rail, or a promoted rail under an id nothing here recognizes, which still renders
  `hub.title` verbatim because there is no substitute catalog for arbitrary server-owned text (and
  no live evidence any of those families need one). `metadata.rs`'s `CollectionShelf` is the one
  Hub-title consumer that deliberately does NOT go through this table: a collection's own name is
  server-owned exactly the way a movie's title is, not a standard shelf heading, so it keeps
  `h.title.clone()` verbatim by design. See `docs/pms-api.md` §3/§3a for the full table and the
  tests (`pms_multi_source_merge_tests.rs`:
  `a_recently_added_library_hub_renders_the_be_catalog_string_under_a_be_ui`,
  `one_movie_and_one_tv_library_get_the_natural_per_type_recently_added_titles`,
  `a_lone_movie_library_renders_the_be_per_type_catalog_string_under_a_be_ui`,
  `an_unrecognized_hub_identifier_keeps_the_pms_title_verbatim`; `section_hubs.rs`:
  `a_be_ui_localizes_the_section_recently_added_hub_and_leaves_an_unknown_one_alone`); don't infer
  a different PMS-side fallback chain from one field report without a live server to verify it
  against.
- **`Connection.local` does not mean what it looks like, and the cost is a probe deadline.** It means
  "this address is RFC1918", NOT "you are on that LAN" — a share advertises the *owner's*
  `172.20.x.x`. `publicAddressMatches` is the field that means the latter. For an unmatched
  non-owned connection, `probe.rs` retains only an advertised HTTPS URI and suppresses plaintext:
  TLS plus the `/identity` response can authenticate the answer, while plaintext could succeed
  against a different machine at the same address on our LAN. That is also why a probe must
  **verify `machineIdentifier` on the response** before accepting a connection, and why `probe.rs`
  is pure (no socket, no clock): the rules are then gradeable on the dev Mac.
- **A ceiling is answered by the transcode FLAVOR *first*, and only then by a parameter.** Plex's
  relay is a ~2 Mbit/s tunnel, so `transcoder::link_policy` denies direct play *and* the container
  remux over one — the remux is the half that is easy to miss, since it copies the codecs and
  deliberately sends no cap, i.e. the same bytes at the same rate one layer down. **No relay
  connection has ever been observed by this codebase**, so that policy is reasoned, not measured;
  read its doc before touching it.
  `TranscodeSpec` grew a `ceiling` field on 2026-08-23 and this bullet used to end "and must not
  grow one" — which was right about a LINK ceiling and wrong as a general rule. A **user-chosen**
  ceiling (`route::Quality`, the picker in the player's `…` menu, for LG checklist #43 CASE1) is a
  different input with the identical mechanism: `route::quality_policy` returns the same two flags
  `link_policy` does, `route::flavors_allowed` composes them so the stricter wins, and only once
  direct play and the remux are BOTH denied does a rate reach the WIRE. Note the field itself rides
  EVERY spec, remux included — `transcode_query` reads it on the re-encode branch alone, and the
  remux branch's silence is an invariant a host test pins, not a `None` you can rely on. The rule that
  survives unchanged is the ORDER: a number that is not preceded by a flavour refusal does nothing
  at all, because the flavour it would have bound is the one with no encoder in it.
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
- **The session file has FOUR writers and exactly one door for changing part of it.** `session.rs`'s
  `update(|s| …)` does the read-modify-write under the module's own lock and writes through a
  sibling tmp + `rename`; `save` is a whole-file REPLACE for the two flows that own the entire file
  (sign-in, profile switch). Reach for `update` for anything that touches one field — the roster,
  the search terms — because the others are workers and the two failures are both silent: a lost
  update resumes the next boot as the wrong profile, and a torn `O_TRUNC` write is an unparseable
  file, which is a QR code on the next boot rather than a stale roster. The lock (`IO`) is held
  across the write's `sync_all`, so **nothing per-frame may take it directly** — but a per-frame
  reader may now call `session::peek()`, because `peek()` is backed by a live in-memory read
  cache, not a per-call file transaction.
- **Every preference key but `language` lives in ONE struct, `session/prefs.rs`'s `Preferences`**
  (the ten player settings' and `trailer_autoplay`'s FROZEN top-level keys, `profile_prefs` (their per-profile home, below), `auto_sign_in`, `last_hero_blur`, `hold_hint_taught`, the account-bound
  `plaintext_consent` and `server_key_pins`, the per-profile `last_library` and `library_sorts` (cleared
  at sign-out like every `ACCOUNT_BOUND = true` row in `prefs::DESCRIPTORS`, but their records carry no
  account stamp, so the one login that is not a sign-out, a stored session that could not be activated
  in `app/boot.rs`, would leave them to the next account: `replace_after_reauthentication_guarded_with_outcome`
  clears both, and the Home pins and recent searches, when the login's owner identity (`owner_identity`, the roster admin's plex.tv id; never the token, which every sign-in mints anew) differs from the stored one or either is unknown), and the two memory-door lists `track_memory` and `subtitle_offsets`), flattened (`#[serde(flatten)] prefs`) into BOTH on-disk
  forms — the legacy file's `Session` and the canonical preference record — declared before each
  form's own `extensions`, so the flatten takes its keys first and the unknown-key map still sees
  only keys nobody models. A new key of that kind is a `Preferences` field (its serde attributes
  written out where rustfmt and a reviewer see them, copied from a neighbour, never unified: each
  row keeps its own soft-parse policy), a marker (`setting!` for a `Copy` value, hand-written when
  `Stored` != `Value` as for quality, `list_row!` for a list) and a line in `prefs::DESCRIPTORS`.
  Callers read `session.prefs.<field>` or the hand-written getter, and change a value with
  `session.with_pref::<Marker>(v)` (a `Copy` row; it writes the TOP-LEVEL key, so for the ten
  per-profile settings it is a fixture builder for the legacy fallback, never what a pick does).
  **The ten player settings and hero trailer autoplay are per profile** (`prefs/profile_prefs.rs`): a profile's own picks sit
  in `profile_prefs` (`[{user, account, prefs}]`, `account` the `grant::account_key` stamp, the owner
  `user: ""`), and what a profile reads is `stored_effective` = its own value, else the frozen
  top-level key (`Setting::get`, a garbage own value included), else the default. The top-level keys
  are never copied and never written by a pick (`persist_profile_pref` is the only writer; it
  refuses without a seat, across a tenure or credential change and with an empty stamp, and prunes
  records of other accounts). `profile_prefs` is account-bound (`ACCOUNT_BOUND_PREFERENCES`, so
  sign-out drops it and the legacy keys stay). Who is seated is `session::Seat`
  (`Seat::of(profile, stamp)`, `current_seat()`); the live cells follow it in `plx_media::route`.
  `hold_hint_taught` is Install and never account-bound, so sign-out keeps it: which hold hints
  (`card`, `pill`) this television has been taught; the application seeds `plx_ui::hold_hint` from
  it at boot and writes it on the frame tick (`app/hint_taught.rs`, `session::learn_hold_hint_taught`).
  `auto_sign_in` is Install and never
  account-bound: `boot_shows_picker` reads it before any profile is seated. `trailer_autoplay` is
  NOT Install any more (P1c): the DETAIL page's hero reads the seated profile's value, the top-level key
  is its frozen fallback, and `preview::enabled()` reads the cell (seeded ON). The `trailer_autoplay`
  FIELD's derived default (here and in `Session`) is OFF while an ABSENT key reads on, so the canonical
  record's own `Default` sets it. `plaintext_consent` is written as `[]` in the legacy file but
  omitted from the canonical record while empty (`split_public` drops the key). `prefs_tests` holds
  the table to the struct (T-fields), to platform's `ACCOUNT_BOUND_PREFERENCES` (T-account) and to
  each row's parse policy (T-parse), both ways; a field with no row, or a row with no field, fails
  there.
  The two memory-door lists (`track_memory`: each profile's remembered audio and subtitle picks per
  show, `prefs/track_memory.rs`; `subtitle_offsets`) have no `Copy` value and no whole-list accessor:
  their markers implement `Row` only (not `ListRow`), and they are written only through
  `ProfileMemory::upsert_with`. `track_memory` is account-bound (no stamp, no write; a write prunes
  every other account's record), its entry strings are bounded (an over-long one reads as absent, a
  title is cut), and it is capped at 64 entries a profile and held to a 32 KiB HOUSEHOLD budget by
  eviction, never refusal. It is READ at resolve time (`route::recall_all` into `ResolveEnv::memory`, and
  only for the SEATED profile: `recall` and `recall_all` ask `session::is_seated_as`, so an unseated
  boot or a dev token reads none; matched by `route/track_memory.rs`, ranked above the PMS selection by `plan.rs`)
  and WRITTEN by the track menu: an explicit pick on an episode carries its descriptor
  (`TrackCommit`'s `memo`), `route::commit_audio_pick` / `commit_subtitle_pick` call `route::remember`
  when the route accepted the pick, and nothing else writes it (a movie, an unseated boot, an
  automatic choice and a harness setup commit remember nothing; the harness's
  `plxnative-trackmemory=clear` removes entries through the same door, and `=fill[:N]` writes N worst-case
  shows through it to read the DB8 write cost at the budget). A remembered Off beats the
  account's rule and PMS's embedded selection, but not a server-selected sidecar the app can draw
  (`plan.rs` `pick_subtitle` withholds the memory from the ladder while the item has one).
  The subtitle half is one of THREE states (`SubtitleMemo`: `Track`, `Off`, `ForcedOnly { lang }`);
  **Forced only** is the Subtitles menu's second row (`TrackCommit::SubtitleForcedOnly`,
  `route::commit_forced_only_pick`), written on an explicit change of mode and never on an OK on the
  row that already holds it (which recommits what is on screen), with the drawn track's language
  (else the playing audio's, else the one the session fell back to). It ranks exactly as a remembered
  Off does: a retry's own pick and a server-selected sidecar the app can draw still outrank it (the
  withholding in `pick_subtitle` is of the memory, whichever of the three it is), and below those the
  next episode shows its forced, non-commentary, renderable subtitle in the
  language of the audio that PLAYS (`route::track_memory::match_forced_only`), or nothing, and never
  falls to a full track or to the lower rungs. `PlaybackSession::sub_mode` (`SubMode`) holds which
  state the playback is in, because a stream id cannot say it (a retry carries it in
  `RetryContext::sub_mode`, and the language the mode falls back to for audio that names none in
  `ResolveEnv::forced_lang`, since a retry consults no memory). Two derived commits keep the mode true, both through the one commit every
  pick goes through and neither writing memory: a new audio re-points it (`commit_audio_selection`),
  and, on a direct play, the first decoded frame re-matches the remembered subtitle once with the
  container's track names (`route::rematch_subtitle_with_names`, called from `player/pump.rs`; an MP4
  part's PMS streams carry no `title`, so only the demuxer's tag says which track is forced). That
  re-match runs BEFORE the write-back's send on the same frame: while a write-back is parked it
  re-points the parked write's subtitle half (`route::repoint_parked_subtitle`, W5 and W7 apply to the
  re-matched id) and queues no PUT of its own, so the server hears one write that names the remembered
  audio and the re-matched subtitle; with nothing parked, or a parked write that went stale first
  (`decision::memory::parked_write_stale`: the route was replaced, the viewer picked), it PUTs like any subtitle
  commit, except that over a Part with an external stream (W5) it PUTs nothing. It never
  replaces a sidecar on screen, and compares with what the landing SHOWS (read from the session after
  the sidecar restore, `settle_landing_selection`), not with the plan, which names no sidecar. Leaving
  Forced only is a remembered change even onto the very track it drew.
  `subtitle_offsets` is account-bound as well (since SO) under different rules: a record without a
  stamp is read as the signed-in account's own and is stamped at the list's next write
  (`MemoryEntry::ADOPTS_UNSTAMPED`), at the plex list a write with no account token is still
  accepted, kept unstamped, and prunes nothing (the media door refuses it: a seat needs a non-empty
  stamp), and a record stamped for another account reads as nothing and is
  pruned by the next write. The media half (`route/prefs.rs` `remember`) stamps its mirror write with
  the stamp the mirror was seeded under (`decision_memory_tests`
  `a_retune_or_a_forget_reaches_the_stamped_record_the_restore_seeded`).
- **The contract for a new setting.** Step 4 is a review rule: no test or CI check enforces it.
  1. **Add the field** to `Preferences`, with explicit serde attributes. Pick the parse policy (a
     soft scalar, `de_soft_vec` for lists, `de_soft_bool_on` when absence or garbage means on) and
     the skip predicate.
  2. **Add the `setting!` line** (`list_row!` for a list, a hand impl when `Stored` != `Value`), and
     add it to `DESCRIPTORS`. Choose:
     - **Scope.** `Install` describes the TV (its decoders, its panel only if the owner says so).
       `Profile` is a person's habit, and is the default for a new player setting. `PerItem` and
       `PerShow` are memories for content. The scope alone decides whether it persists across episodes.
     - **Account binding.** `Account` rows are always bound, `Install` rows never; T-account holds both.
     - **`LIVE`**, whether the media layer applies it to a live cell.
     - **The settings write.** Optimistic (`How::Optimistic`) only when the viewer watches the effect
       while choosing.
  3. **If `LIVE`,** add a `live_cell!` and a `LiveSetting` impl (`normalize`, `after_saved`) to
     `LIVE_ROWS`. T-live fails until both sides agree.
  4. **Write only through the door.** `route::choose` for a profile's value (it takes a
     `LiveSetting`, and only `Profile` rows have a `ProfileSetting`; an `Install` value has no
     `choose` door and goes through its own ticketed writer, `queue_update_ticket`),
     `route::remember` for a memory. `queue_update_settled` is for an edit whose landing the caller
     reports (`plex::grant::persist` does); do not use it to claim "saved" before the write has
     landed. Never add a second restore list (`restore_install_rows` / `restore_profile_rows` are
     the only ones).
  5. **Schema rules.**
     - Never rename a key. A change of meaning gets a new key.
     - New data goes only inside `public.preferences` (DB8) or the legacy file's top level. Never add
       a `PublicPayload` slot, a state or auth field or a `Mutation` variant: they are
       `deny_unknown_fields`, and an older build would sign the user out.
     - New keys are additive, and a new list is soft-parsed per element (`de_soft_vec`). T-parse
       needs a fixture for it, and the A0 characterisation needs a real-shape case.
     - A new list that can grow has a byte bound, checked against the 256 KiB document (next
       bullet); the document guard's eviction order (`prefs::shed_memory`) names every list it may evict.
     - An EXISTING struct keeps its flatten `extensions`; a new struct needs none.
     - Omit a value at its default (`skip_serializing_if`). Fields of `Preferences` without one are
       written at their default, kept that way for behaviour preservation; read them from the
       struct, and a new field follows the omit rule.
  6. **The Settings row** (labels, i18n, picker) is a screens edit, and `Choice` gains a variant
     (T-settings).
- **The DB8 document may not pass `MAX_LOGICAL_BYTES` (256 KiB), and per-profile memory is what
  grows it.** Past the cap EVERY write fails (`LogicalTooLarge`, reported as `InvalidSchema`), so
  `persistence::commit_session_with` measures the projected document (platform's own
  `state::logical_size`, with the stored envelope, ledger, consent and future keys in hand, for the
  write it is really about to send: update, one-put replace, or the strict staged write; a first
  write onto an ABSENT record, a sign-in or a legacy import, is measured against the fresh state the
  helper starts from; the sealed credentials are sized as the helper will seal them, `Seal`: ACL-only,
  keymanager, or whichever of the two is larger when a fallback is permitted) and, if it
  would not fit, sheds from the payload `track_memory`, then `subtitle_offsets`, then
  `library_sorts` (`prefs::shed_memory`; the same oldest-from-the-largest-record rule as the
  household budget), only as far as it needs, and logs `prefs: evicted <n> <list> entries to fit
  the 256 KiB document`. The live session keeps what the payload lost until the next load. It never
  refuses a write: with nothing sheddable left the platform still does, as before. The file backend
  has no cap and no guard. A stored document already AT the cap leaves no room for the strict tier's
  second copy (old and new halves side by side), which the guard cannot create. A record created on
  webOS 4.x is ACL-only; a record that already holds a keymanager envelope still stages.
- **`session::peek()` is a write-through cache over the persisted session, not a fresh read.** It
  is a pure read of that cache: one uncontended `Mutex` lock and an `Arc<Session>` clone — no `IO`,
  no helper round trip, no submit, no clock, on a hit or a miss — which is what makes it safe to
  call every frame. The invariants
  that keep it correct, all in `session.rs`'s module doc and worth knowing before touching either
  the cache or a write path:
  - Cached records come from completed reads or durable writes under `IO`. `Revoked` separately
    suppresses credentials immediately during a queued sign-out, even if its disk clear fails.
    Reads and preference edits preserve it; only an explicit proven credential write ends it.
  - Only `session.rs` writes the session domain of the record; every `persistence::commit_*`/
    `write_session`/`commit_cleared` caller updates the cache through a read install, proven-write
    install, cache drop, or local revocation. Read installs cannot undo revocation.
  - `peek` never takes `IO` and never refills the cache, including on a miss: it returns the
    previous snapshot (or the empty session). The refill belongs to the session owner
    (`SessionMachine`): the application's frame step asks `refresh_need()` what the cache lacks
    and delivers `SessionEvent::CacheObserved`, the owner answers with its `SessionFx::RefreshCache`
    effect, the application's session adapter runs `refresh_cache_job` on `storage_worker`'s
    bounded FIFO, and the outcome returns as `SessionEvent::CacheRefreshed` (one refresh in
    flight, owned by the machine rather than a `peek` side channel). The job reads and installs
    under `IO`, then advances a visible-session generation only if the served content changed. The
    bridge observes it and invalidates on the frame thread. Cached views also observe it through
    `VisibleSessionWatch` (or include `visible_generation()` in their cache key) and rebuild;
    invalidating drawing alone cannot refresh a retained value. `peek_settled()` keeps transient
    reads distinct from an authoritative empty session. Unchanged retries remain quiet. A queued
    read cannot overwrite a newer write or sign-out.
  - Writers never read the cache to decide what to write — they always re-read the authority under
    `IO` first (the fence/OCC check), then install their own proven outcome. A miss can therefore
    never overwrite a newer concurrent write.
  - A `Locked`/`Blocked` read (keymanager unavailable, a helper hiccup) is cached only
    transiently: `peek` serves the last known state and the owner re-reads it no sooner than
    `LOCKED_RETRY_MS` (1000) of frame time later, per cache generation (a backoff counted on the
    frame tick the owner receives, not a wall-clock deadline in `session.rs`) — never latched
    forever the way a naive per-field cache once was (the bug PR #120's stopgap shipped and this
    cache replaced).
  - Sign-out (`clear()`) drops the cached `Arc` immediately; the tokens it held must not remain
    reachable in memory after a sign-out just because nothing had overwritten the cache yet.
  - `plex::session::async_persistence`'s Stage B coordinator is unwired and keeps its own,
    separate `CACHE` today; when it is wired up, it must install into/drop the cache above
    (`install_locked`/`drop_cache_locked`, under `IO`) instead of maintaining a second copy of the
    session.
- **Track selection is server-side, via `PUT /library/parts/{id}`** (set the chosen audio/subtitle
  stream + subtitle burn), **not** query params on the stream URL. The server re-selects for the next
  decision; the client re-requests the part. See `[[audio-subtitle-track-switching]]`.
  `StreamSelection.subtitle_stream_id` is an `Option<i64>`: `Some(0)` is Off, `None` omits the
  parameter, which an audio-only write-back uses so it never creates an explicit Off (PMS deletes a
  downloaded subtitle once the selection moves off it, `docs/pms-api.md`).
- **Subtitles: assume client rendering, not the server.** WebVTT sidecars do **not** deliver on our
  progressive-MKV pipeline (they come back empty / 501), so during a transcode the only server option
  is **burn** — which is why direct-play + our own subtitle renderer (see `player/`) is the real
  answer. Don't add a soft-subtitle path that only works on paper. See `[[soft-subs-during-transcode]]`.

## Where the bytes go next

This layer only *decides and locates*; the actual byte stream (direct-play or transcode) is pulled
through `stream.rs` or `curlio.rs`, demuxed by `ff.rs`, and fed to Starfish by the `player/` engine
— see `player/CLAUDE.md` for the Load-payload/codec and subtitle-rendering rules on the other side
of the handoff. The old hand-rolled `mkv.rs` path is retired.
