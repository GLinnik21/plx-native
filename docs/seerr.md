# Seerr

Requesting movies and shows through the household's own Seerr server (Overseerr, Jellyseerr and
Seerr share one `/api/v1`). This file is what the code does; where it and a plan differ, this file
is right.

**The rule that frames everything: Seerr is neither plex.tv nor a PMS.** It is a server the
household runs, at an address the owner typed. Nothing is sent to it until an address is set, and
it is reached at exactly that address.

## Where it lives

| piece | module | role |
|---|---|---|
| transport | `net::Resp::cookies` | each `Set-Cookie`'s `name=value`, beside `ETag` |
| client | `plex::seerr` (`SeerrClient`, `live_transport`) | sign-in, search, discover rows, a title's detail, my requests, `POST /api/v1/request`; injectable transport; the dev-build write failsafe |
| address | `Preferences::seerr_address` (`session::set_seerr_address`) | install-wide, kept across sign-out, no credential |
| job body | `data::seerr` (`SeerrOp`, `run_op`, `SeerrIo`) | signs in when it holds no session, once more after a 401 |
| store | `stores::seerr::SeerrStore` (`StoreId::Seerr`, `StoreJob::Seerr`) | the connection and every surface's answer, per `Slot`; only a slot's latest ask lands |
| app | `Bridge::seerr_pump` (once a frame), `Bridge::seerr_connect`, the runner's `seerr-call` | hands the store the stored address, signs a seated profile in once |
| Settings | `screens/src/settings_seerr.rs` (Services > Seerr) | address field (`plx_ui::text_field`), Connect with the live connection line, Remove |
| Requests tab | `screens/src/requests.rs` (`AppArg::Requests`, `TAB_KEY_REQUESTS`) | on the strip between the Watchlist mark and Search while an address is set (`chrome::StripSource::requests`); rows My requests (the first `MINE_SHOWN`, each title read by its own `Detail`), Trending, Popular movies and shows, Upcoming movies and shows; a card opens the Seerr title page |
| Detail page | `screens/src/detail` (`HeroCtl::Request`, `RequestFace`, `request_face`) | a show's missing seasons: the last disc of the hero row (see below) |

What a title page's pill and a show's request disc offer is ONE decision, the store's
`stores::seerr::offer` (a movie nobody asked for, or a show's seasons the server neither holds nor
was asked for; nothing while a request is in flight or after one went through).

Layers: the client is `plex`, the store `data` (no worker: its jobs are started by the app's
runner, docs/sans-io.md), the page `screens`, which reads the store through `InnerViews::seerr`.

## Sign-in and the session

`POST /api/v1/auth/plex {"authToken": <the seated profile's plex.tv token>}` signs that Plex account
in; Seerr answers with the user and a `connect.sid` cookie, sent back as `Cookie:` on every later
call. The session is never stored: it lives in the store, goes with the profile it was signed in
for, and is signed in again after a 401 (a server restart forgets sessions). A 403 at sign-in is a
Plex account the server does not let in (not imported, or Seerr's "new Plex sign-in" off); Settings
says so. Redirects are never followed: a cookie must not reach another host.

## A show's missing seasons (the Detail page)

A show's page asks Seerr about the show once per page (`Slot::Detail(Tv, tmdb)`), when its server
sent a TMDB id (`tmdb://` in the item's `Guid[]`, `Metadata::tmdb_id`, read with `includeGuids=1`)
and an address is set. When seasons are missing the hero row ends with a request disc
(`HeroCtl::Request`): a plus that unfurls to "Request missing seasons", and a press asks for every
one of them in one request. Its face then says what became of it, through its mark and verb: Requesting…,
Requested (a check), Already requested (a check, a 409), or why it failed (a 403 or no answer),
when a press tries again. A show with nothing missing, a show without a TMDB id, a movie, a season
and an episode have no disc. The page asks again whenever it is opened again; while that answer is
on its way the disc is absent.

## Wire facts (Overseerr's `overseerr-api.yml`; not yet checked against a live server)

- Lists: `{page, totalPages, results}`; rows have `mediaType` `movie`/`tv`/`person` (people are
  dropped), `title` on a movie and `name` on a show, TMDB image paths in `posterPath`/`backdropPath`.
- Availability, `mediaInfo.status`: 1 unknown, 2 pending, 3 processing, 4 partly available, 5
  available, 6 blocklisted. A row with no `mediaInfo` is a title nobody asked for.
- A request: 1 pending approval, 2 approved, 3 declined. A show's season state folds what the server
  holds (`mediaInfo.seasons`) with what open requests asked for; specials (season 0) are left out.
- `GET /api/v1/request?take&skip&filter=all&sort=added&requestedBy=<id>`: the user's own requests;
  the rows carry the TMDB id but no title.
- `POST /api/v1/request {mediaType, mediaId, seasons?}`: 201; 409 when already requested; 403 when
  the user may not (no permission, over quota).
- Every search term is percent-encoded byte for byte (Overseerr refuses a raw quote or space).

## The write failsafe and the stand-in

A `devtriggers` build refuses `POST /api/v1/request` (the one call that starts a download) unless
the loopback stand-in or the owner's live switch is armed:

- `plxnative-seerr=http://127.0.0.1:<port>` replaces the configured address with
  `tests/mock_pms.py`'s Seerr (`SeerrFixture`, same listener). Anything not loopback is refused.
- `plxnative-seerr-live` lets a debug build write to the owner's real Seerr. **No agent creates it
  unless the owner asks in that session**; `.claude/hooks/watchlist-live-guard.py` refuses a command
  that would.

The boot logs `seerr: base=stand-in|configured writes=open|refused`; `seerr_stand_in_problem` in
the mock reads it. Logs name the method, the PATH shape and the status only — never the host, a
query, a title, the token or the cookie.

## Known limits

- Managed Plex Home profiles may not be allowed to sign in to Seerr; Settings shows the refusal.
- Requests use the server's defaults: no 4K, quality profile or server choice.
- "My requests" shows the first page of the list, newest first, and only its first `MINE_SHOWN`
  titles: Seerr's request list carries no title, so each card costs one more call. It is read
  again whenever the tab is uncovered.
- A show's request asks for every missing season at once: there is no season picker, on the Detail
  page or the title page.
- A show's TMDB id comes from its PMS record. A show matched by an agent that records none (the
  legacy TheTVDB agent, or a show matched by nothing) has no request disc.
