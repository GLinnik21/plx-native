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
| client | `plex::seerr` (`SeerrClient`, `live_transport`) | sign-in, search, discover rows, a title's detail (with TMDB's facts, its downloads and its requests), the request list, the Radarr / Sonarr servers and one server's profiles and folders, `POST /api/v1/request`, approving and declining; injectable transport; the dev-build write failsafe |
| address | `Preferences::seerr_address` (`session::set_seerr_address`) | install-wide, kept across sign-out, no credential |
| job body | `data::seerr` (`SeerrOp`, `run_op`, `SeerrIo`) | signs in when it holds no session, once more after a 401 |
| store | `stores::seerr::SeerrStore` (`StoreId::Seerr`, `StoreJob::Seerr`) | the connection and every surface's answer, per `Slot`; only a slot's latest ask lands; a `SeerrCmd::Refresh` keeps a slot's shown answer while it is read again, and when that read fails |
| app | `Bridge::seerr_pump` (once a frame), `Bridge::seerr_connect`, the runner's `seerr-call` | hands the store the stored address, signs a seated profile in once |
| Settings | `screens/src/settings_seerr.rs` (Services > Seerr) | address field (`plx_ui::text_field`), Connect with the live connection line, Remove |
| Seerr tab | `screens/src/seerr_tab/` (`AppArg::Seerr`, `TAB_KEY_SEERR`) | on the strip after the type pills and just before the Watchlist mark while an address is set (`chrome::StripSource::seerr`); a header row switches its two sections, Requests (the request list as `plx_ui::media_row` rows) and Discover (Trending, Popular and Upcoming shelves); see below |
| Detail page | `screens/src/detail` (`HeroCtl::Request`, `RequestFace`, `request_face`) | a show's missing seasons: the last disc of the hero row (see below) |
| request menu | `screens/src/seerr_request.rs` (`ContentPanel::SeerrRequest`, `AppArg::SeerrRequest`) | where a request goes: server, quality profile, root folder (see below) |

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

## The Seerr tab

One page, two sections under the Library's header row (`plx_ui::page_header`): the section pills
"Requests" and "Discover" stand on the left, the one on view on the selected plate, and OK on the
other switches in place. The page opens on Requests.

- **Requests** is the request list (`Slot::Requests(page)`), everybody's for a user the server shows
  them to and the user's own otherwise; the header's read-out says which ("Everyone's requests",
  "Your requests", from `SeerrUser::sees_all_requests`). Each request is a row
  (`plx_ui::media_row::MediaRow`, its words composed in `seerr_tab/rows.rs`): the poster and the
  title (read by the title's own `Detail`, since the list names none), the kind, year and requested
  seasons, who asked and when, then the download while it moves ("68% · 1.2 GB of 4.0 GB · 12m
  left", with the amber band along the poster's foot) or who approved or declined it, and a chip for
  what became of it (Pending, Approved, Declined, Failed, Downloading, Processing, Partly available,
  Available). A row is a section of its own, so the page scrolls to the focused row as it does to a
  shelf. OK on a row opens the title's Seerr page, where a request manager approves or declines it
  (below).
- The list is read again when the page is uncovered or Requests is chosen, and every 30 s
  (`REFRESH_MS`) while a request on view is approved and not yet available; a refresh
  (`SeerrCmd::Refresh`) keeps the rows on screen until it lands. Rows keep their order while the
  page stands: a refresh updates them in place, and a request new to the page goes on top, so the
  server re-sorting by its change time never moves the row under focus. The next page is read as
  focus nears the last rows, so every request is reachable.
- **Discover** is the five suggestion rows as shelves of the Request card. Titles wholly on the
  server are left out unless the header's "Available · Hidden / Shown" control shows them (a partly
  available show stays). A shelf the filter leaves short reads up to three pages by itself
  (`MAX_FILL_PAGES`), and every further page as the reader walks near its end.

## A title's page

`screens/src/seerr_title.rs`, opened by a Discover card, a Search Request card or a request row.
Under the title it says what Seerr knows: TMDB's facts (kind, year, runtime, TMDB's score as a
percentage, two genres, the network or studio), the availability (or what the last request or
decision said), the newest request (its state, who asked and when, who approved or declined it),
the download while one moves (with the amber band) and, for a show, one chip per season with its
own state (Available, Partly available, Pending, Processing, Missing, Blocked). The words a request
is described by are written once, in `registry::seerr_cards`, for the tab's rows and this page.

The controls stand in one row: the Request pill while there is something to request, then
**Approve** and **Decline** for a user with Manage Requests (`SeerrStore::may_manage_requests`)
while a request of the title waits for approval. They answer the newest waiting request
(`Slot::Decide(id)`, `POST /api/v1/request/{id}/approve|decline`); while it is on its way the
pressed one says Approving… or Declining… and a second press sends nothing. A decision that went
through, or one the server says somebody already made (409), reads the title back once; a refusal
(403) or no answer says so in the status line and leaves the buttons to try again. Going back to the
Seerr tab reads the list again, so the row shows the new state.

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

## Where a request goes (the request menu)

For a user Seerr lets choose (its admin, Manage Requests or Advanced Requests permission:
`SeerrUser::may_choose_options`, read from the sign-in's `permissions`), the title page's Request
pill and a show's request disc open a menu anchored to them instead of sending. Its first row,
Request, sends; under it the servers to choose from (only when there is more than one non-4K one),
then the chosen server's quality profiles and root folders, each folder with its free space. Every
row starts checked on what the server would pick unasked: its default non-4K server
(`default_server`), that server's default profile and folder, a Sonarr server's anime pair for a
show TMDB files under the anime keyword (`TitleDetail::anime`). Choosing another server starts its
profile and folder over on its own defaults. Request names only what was changed
(`seerr_request::request_options`), so a row left alone stays the server's to decide, override rules
included. It is sent on the title's own `Slot::Request`, so the page's pill or disc follows its
answer as it follows its own press. A user without the permission sends at once, as before, and
Seerr applies its defaults.

The menu reads `GET /api/v1/service/{radarr|sonarr}` (`Slot::Servers`) when it opens and
`GET /api/v1/service/{radarr|sonarr}/{id}` (`Slot::ServerOptions`) for the server it stands on.
Until they land, or if they cannot be read, the menu says so in a note and Request still sends.

## Wire facts (Overseerr's `overseerr-api.yml`; not yet checked against a live server)

- Lists: `{page, totalPages, results}`; rows have `mediaType` `movie`/`tv`/`person` (people are
  dropped), `title` on a movie and `name` on a show, TMDB image paths in `posterPath`/`backdropPath`.
- Availability, `mediaInfo.status`: 1 unknown, 2 pending, 3 processing, 4 partly available, 5
  available, 6 blocklisted. A row with no `mediaInfo` is a title nobody asked for.
- A request: 1 pending approval, 2 approved, 3 declined, 4 failed, 5 completed
  (`server/constants/media.ts`). A show's season state folds what the server holds
  (`mediaInfo.seasons`) with what open requests asked for; specials (season 0) are left out.
- `GET /api/v1/request?take&skip&filter=all&sort=modified`: the most recently changed requests first.
  The client names no `requestedBy`: the server answers everybody's to a user with Manage Requests
  or View Requests (an admin has both) and only the caller's own to anyone else
  (`server/routes/request.ts`). The rows carry the TMDB id but no title, and their `requestedBy` and
  `modifiedBy` users (`displayName`), `createdAt`, `updatedAt` and `media`, whose `downloadStatus` is
  Radarr / Sonarr's queue for the title right now: `[{title, size, sizeLeft, status, timeLeft
  ("[d.]hh:mm:ss"), estimatedCompletionTime, episode?}]`, empty when nothing is downloading
  (`server/lib/downloadtracker.ts`, filled when a `Media` row loads).
- `GET /api/v1/movie|tv/{id}` also carries TMDB's `voteAverage`, `genres`, `status`, a movie's
  `runtime` and `productionCompanies`, a show's `episodeRunTime` and `networks`, and in `mediaInfo`
  the title's `downloadStatus` and its `requests`, each with its users and dates.
- `POST /api/v1/request/{id}/approve|decline`: Manage Requests only (403 otherwise); a request that
  is no longer pending answers 409; 200 is the request as it now is.
- `POST /api/v1/request {mediaType, mediaId, seasons?, serverId?, profileId?, rootFolder?}`: 201;
  409 when already requested; 403 when the user may not (no permission, over quota). For a user
  without Manage Requests or Advanced Requests the server's override rules replace the last three.
- `GET /api/v1/service/radarr|sonarr`: `[{id, name, is4k, isDefault, activeProfileId,
  activeDirectory, …}]` (Sonarr's also `activeAnimeProfileId`, `activeAnimeDirectory`); `GET
  /api/v1/service/radarr|sonarr/{id}`: `{server, profiles: [{id, name}], rootFolders: [{id, path,
  freeSpace, totalSpace}], tags}` (Seerr's `server/routes/service.ts`). Any signed-in user may read
  them.
- Permission bits (`server/lib/permissions.ts`): ADMIN 2, MANAGE_REQUESTS 16, REQUEST 32,
  REQUEST_ADVANCED 8192, REQUEST_VIEW 16384. A show's anime-ness is TMDB keyword 210024 in the detail's `keywords`.
- Every search term is percent-encoded byte for byte (Overseerr refuses a raw quote or space).

## The write failsafe and the stand-in

A `devtriggers` build refuses every `POST` under `/api/v1/request` (a new request, an approval or a
decline: the calls that start a download) unless the loopback stand-in or the owner's live switch is
armed:

- `plxnative-seerr=http://127.0.0.1:<port>` replaces the configured address with
  `tests/mock_pms.py`'s Seerr (`SeerrFixture`, same listener). Anything not loopback is refused.
  Its user is an admin (the request menu opens); `POST /_mock/seerr {"advanced": false}` makes it a
  plain requester, whose press sends at once, and `{"manage": false}` a user with Advanced Requests
  but not Manage Requests (the request list is their own only, and decisions answer 403). Two
  other users have requests from the start: one waiting for approval, one approved with a download
  moving, one declined and one completed.
- `plxnative-seerr-live` lets a debug build write to the owner's real Seerr. **No agent creates it
  unless the owner asks in that session**; `.claude/hooks/watchlist-live-guard.py` refuses a command
  that would.

The boot logs `seerr: base=stand-in|configured writes=open|refused`; `seerr_stand_in_problem` in
the mock reads it. Logs name the method, the PATH shape and the status only — never the host, a
query, a title, the token or the cookie.

## Known limits

- Managed Plex Home profiles may not be allowed to sign in to Seerr; Settings shows the refusal.
- Requests are never 4K: a 4K server is not offered, and there is no 4K request.
- The menu has no tag or language-profile choice: the server's defaults apply.
- Seerr's request list carries no title, so each request row costs one more call (its title's
  detail), made once per page as the page arrives.
- Request dates are the request's own UTC day in the language's short form, not "2 days ago": the
  television's clock can be wrong after an offline start, and the UI has no wall clock.
- A show's request asks for every missing season at once: there is no season picker, on the Detail
  page or the title page.
- Approve and Decline answer the newest waiting request of a title, on its page; the Seerr tab's
  rows open that page rather than offering the decision in place (a menu over the tab cannot open
  a page, and the page is where the request's details are).
- A show's TMDB id comes from its PMS record. A show matched by an agent that records none (the
  legacy TheTVDB agent, or a show matched by nothing) has no request disc.
