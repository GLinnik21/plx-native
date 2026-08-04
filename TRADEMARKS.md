# Trademarks, brand assets, and non-affiliation

The [MIT grant](LICENSE) covers this project's own source code. This file covers the things a
licence grant does not: the marks that identify the project, and whose software this interoperates
with.

It lives here rather than appended to `LICENSE` for a mundane reason worth recording — GitHub
detects a project's licence with [`licensee`](https://github.com/licensee/licensee), which matches
`LICENSE` against known licence texts by similarity. Thirty lines of appended reservation pushed
the file under the threshold, so the repository advertised itself as "Other" instead of MIT. That
misrepresents the terms in the one place most people look. `LICENSE` is now verbatim MIT and
nothing else.

## Trademarks and brand assets

The name **PlxNative** and the PLX logo and splash artwork — `assets/logo-master.png`,
`assets/splash-master.png`, and the `pkg/icon*.png` / `pkg/largeIcon.png` / `pkg/splash.png` cut
from them — identify this project. They are **not** licensed for use as the identity of a derived
or redistributed work.

You may fork and redistribute this software under the MIT terms; please do so under your own name
and mark, so that users can tell the two apart. This is the usual reservation made by projects that
ship an identity along with their code, and it restricts nothing else.

This reservation rests on **trademark** — on the marks identifying the origin of this application —
and is deliberately not framed as a copyright claim. The master images were produced with a
generative image model, and the extent of copyright in such output is unsettled and, in some
jurisdictions, nil. That affects a copyright claim; it does not affect the marks, whose protection
comes from use in commerce rather than from authorship.

## Third-party components

This software links against and bundles code owned by others, under their own licences — including
LGPL-2.1 components whose terms grant you rights neither `LICENSE` nor this file does. See
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md), which travels inside the distributed package as
well as in this repository.

## Plex

This is an unofficial, third-party client. It is **not** affiliated with, endorsed by, or sponsored
by Plex GmbH or Plex, Inc. "Plex" is their trademark and is used here only to describe what this
software interoperates with.

## Other marks

"LG" and "webOS" are trademarks of LG Electronics; "Rotten Tomatoes" of Fandango Media; "IMDb" of
IMDb.com; "TMDB" of The Movie Database. None of those companies is affiliated with this project.
Where their names appear in the application, they identify whose service or review score is being
shown, and nothing more.
