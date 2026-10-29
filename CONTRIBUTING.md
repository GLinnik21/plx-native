# Contributing

FinNative is a Jellyfin client built on [PlxNative](https://github.com/GLinnik21/plx-native).
Contributions to FinNative go here, to <https://github.com/tiper/fin-native>. A change that is not
about Jellyfin (the player, the interface, the TV integration) belongs in PlxNative itself: open it
there, and FinNative picks it up when it next merges PlxNative.

New contributions to FinNative are offered under GPL-3.0-or-later, the licence of PlxNative and of
FinNative's changes to it, without copyright assignment. Retain existing third-party notices and
identify external code, its source revision, license and modifications. Do not submit code whose
redistribution rights you cannot establish.

Please include a `Signed-off-by: Your Name <email>` trailer certifying the
[Developer Certificate of Origin 1.1](https://developercertificate.org/). This records your right
to contribute; it is not a transfer of copyright or a requirement to re-sign historical MIT work.
Use the project verification workflow in `AGENTS.md`, and preserve unrelated changes.

Translations: see `docs/localization.md`. A new or changed language is merged once a native speaker
has reviewed it; the pull request should say who, and screenshots of the UI help them judge it.
Every string FinNative adds needs every language in `locales/`, or the build fails.
