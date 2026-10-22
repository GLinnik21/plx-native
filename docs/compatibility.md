# Compatibility reports

Which televisions PlxNative has been seen running on, as reported by their owners. For the short
answer and the sets with a known problem, see [Will it work on my
television?](../README.md#will-it-work-on-my-television); this page is the evidence behind it.

Each row is one report, linked to where it was made. **Tried** lists only what the reporter saw
working; everything else is not tested, not known to fail. A model that appears twice has two
different results because the builds or firmware differ, so read the build column.

To add a set, post in [Discussions > Compatibility](https://github.com/GLinnik21/plx-native/discussions/categories/compatibility).
Whether it works or not, the form asks for the same things.

| TV | webOS (platform release) | Build | Install | Result | Tried | Report |
|---|---|---|---|---|---|---|
| LG 75UK6570AUA (2018), board `LM18A_ATSC_KR` | 4.4.3 (4.4.3-22, firmware 05.50.70) | 0.8.0 prebuilt | Developer Mode | Works | Sign-in, browsing, direct play of an MKV, resume, fast-forward | [#882](https://github.com/GLinnik21/plx-native/discussions/882) |
| LG 65UJ6200-UA (2017) | 3.9.0 (shipped with 3.5) | nightly 0.9.0-nightly-20261020 | Developer Mode | Works | Sign-in by QR code, browsing, search, video playback | [#878](https://github.com/GLinnik21/plx-native/discussions/878) |
| LG OLED55C7V (2017) | 3.9.4 (3.9.4-6270915) | Nightly, version not given; 0.8.0 before it | Nightly build | Works on the nightly. On 0.8.0 the reporter saw crashes on the remote's d-pad, in the feed and when seeking, and none since the nightly | Sign-in, browsing, direct play, resume and seeking, a transcoded stream, HEVC or 4K, HDR or Dolby Vision, subtitles, audio track switching | [#900](https://github.com/GLinnik21/plx-native/discussions/900) |
| LG OLED77B5PUA (2025), board `K24N_ATSC` | 10.3.0 | 0.8.0 release build | Rooted, Homebrew Channel | Works | Sign-in, browsing, search, movie playback | [#906](https://github.com/GLinnik21/plx-native/discussions/906) |
| LG OLED55C7V-Z (2017) | 3.9.0 | 0.8.0 prebuilt | Developer Mode | The app closes on Play, with no message | Not recorded | [#249](https://github.com/GLinnik21/plx-native/issues/249) (comment) |
| 2018 sets on the M16P chip | 3.9.3 | 0.6.0 to 0.6.6 (prebuilt) | Developer Mode | Sign-in and browsing work; playback never starts | Opt-in usage reports | [#249](https://github.com/GLinnik21/plx-native/issues/249) |
| LG C7, W17H chassis | 3.9.3 | Source build at or after [`60f99c7`](https://github.com/GLinnik21/plx-native/commit/60f99c7) | Developer Mode | Works | Direct play | [README](../README.md#will-it-work-on-my-television) |
| A 2016 set | 3.4.3 | Not recorded | Developer Mode | Works | Sign-in, browsing, search, direct play, transcodes, subtitles, seeking | [README](../README.md#will-it-work-on-my-television) |
| LG 49SM9000PLA (2019), the development set | 4.5 | Every build | Rooted | Works | The set the app is developed and tested on | [README](../README.md#will-it-work-on-my-television) |

The 3.9.x rows read together: the prebuilt v0.8.0 does not play on 3.9.x firmware, while builds
that contain [`60f99c7`](https://github.com/GLinnik21/plx-native/commit/60f99c7) do, on 3.9.3 (a
source build) and on 3.9.0 (the 20261020 nightly, which contains that commit).

## Adding a row

A maintainer adds one row per report, copying what the reporter wrote rather than interpreting it:
the model as given, the platform release from Settings > About This TV, and the checked items of
the form's "What you tried that worked" list under **Tried**. When a later report contradicts an
older one on the same model, add a row instead of replacing it, and say in the build column what
differs. If a row changes what the README claims, change the README's section in the same pull
request.
