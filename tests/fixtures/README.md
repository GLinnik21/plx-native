# `tests/fixtures` — synthesizing the media the suite needs

`tests/run.py` grades the player against nine symbolic **item shapes**
(`movie_h264_ac3_1080p`, `episode_hevc_4k_hdr10_eac3`, …), and
`tests/manifest.local.json` maps each shape to a ratingKey on whatever PMS you own. That
mapping is the entire barrier to entry, and it is a real one: the shapes include a TrueHD
default track with an AC-3 sibling, a Dolby Vision profile 8.1 base layer, a PGS bitmap
subtitle track, and an eight-track audio file with **English DTS at ordinal 6**. Nobody
has that lying around by accident, and two of those — TrueHD and Dolby Vision — have no
freely-licensed example anywhere in the world, so "go and find one" is not advice, it is a
dead end.

`make_fixtures.py` removes the barrier. It builds every shape from `lavfi` sources with
nothing but ffmpeg (plus `mkvmerge` and `dovi_tool` for the one Dolby Vision shape), lays
them out in two Plex-scannable trees, **reads the finished file back and checks the
properties the suite actually grades** — codecs, resolution, bit depth, colour transfer and
primaries, audio track order and language tags, subtitle cue coverage, Dolby Vision RPU
presence, duration, bitrate floor — and writes a `fixtures.json` describing what it proved.
A contributor with no exotic library can then run **15 of the 21 cases** (the other six need
Plex Pass, and three of those additionally need Plex's marker detector to fire — see
§*What this does not solve*).

Four declared properties are **not** read back, and are documented rather than verified:
`faststart` atom order on the mp4, the sidecar `.srt`'s contents (existence only), the
per-channel audio pitches, and the burn-in pixels — for the last, `verify()` compares the
container *description tag* against the same spec that drew the banner, so a degenerate
overlay would pass. Treat those four as intent, not evidence.

```
python3 tests/fixtures/make_fixtures.py --out ~/plxnative-fixtures
# or, the same thing:  make fixtures            (make fixtures-quick for the smoke run)
```

Roughly 20 minutes and ~3 GB on an Apple-silicon Mac: ten files, nine shapes (the HDR
episode is a pair). The script prints its own ETA and size estimate up front (22 min /
3.1 GB, i.e. deliberately a little high), derived from encode rates measured on an idle
Mac — this is a CPU-bound job, so anything else encoding on the same machine roughly doubles
it. Neither `make`
target is wired into `all` or `check` — generating media is a deliberate act, and no
television is involved in any of it.

---

## The honest caveat, up front

**A green suite run against synthetic media is not the same claim as a green run against
real media.** These files exercise the same code paths — route decision, demux, Starfish
payload, ACB bind, timeline climb — but a real remux carries encoder quirks, odd GOP
structures, container edge cases and metadata this generator will never produce. Read a
pass here as *"no regression against the shapes the suite names"*, never as *"plays the
world's media"*. `tests/README.md` draws the same line around `--fps` numbers; it applies
with more force here, because a fixture is a thing somebody chose, and it will only ever
contain what was thought of.

Two shapes in the suite are still unreachable by anyone, synthetic or not, and stay that
way: **VC-1** has no free encoder in existence, and **Dolby Vision profile 5** needs an IPT
base layer no free tool authors. Those live in `library_gaps` in `tests/manifest.json`.

---

## 1. Install the tooling

```sh
brew install ffmpeg          # required — needs libx264, libx265, libsvtav1, libopus
brew install mkvtoolnix      # optional — only the Dolby Vision shape
brew install dovi_tool       # optional — only the Dolby Vision shape
```

Run the preflight to see exactly where you stand; it names every missing encoder, the
shapes each one costs, and the brew line that fixes it:

```sh
python3 tests/fixtures/make_fixtures.py --list
python3 tests/fixtures/make_fixtures.py --quick --out ~/plxnative-fixtures   # ~75s smoke
```

**Missing optional tooling skips its shapes and prints why — it never aborts, and it exits
0 even when every shape asked for was skipped.** That is the same skip channel
`tests/run.py` uses for an item you have not mapped, and the same house rule: a partial set
that says what it is beats an all-or-nothing failure. Without `dovi_tool`/`mkvmerge` you get
eight of the nine shapes and `dp_hevc_eac3_dovi_p8` skips.

**Three of the audio encoders are EXPERIMENTAL** — `truehd`, `dca` and the native `vorbis`
encoder — and a build can be configured without them, so the skip message for those names
does *not* say "brew install ffmpeg" (you already have ffmpeg; that was the thing that just
failed). Check with `ffmpeg -hide_banner -encoders | grep -E ' truehd| dca| vorbis'`;
Homebrew's ffmpeg normally has all three. Losing them costs
`movie_hevc_4k_hdr10_truehd` and `movie_h264_ac3_many_audio`.

**SUPer is not needed.** The one genuine gap in ffmpeg — it has a PGS *decoder* and no PGS
*encoder*, and refuses text→bitmap conversion outright — is closed inside this script by a
small HDMV PGS writer. Verified by feeding the result back through ffmpeg's own PGS
decoder, which reports one rect per cue and zero on the clear: exactly the display-set
stream `subtitle_image_pgs` grades.

## 2. Generate

```sh
python3 tests/fixtures/make_fixtures.py --out ~/plxnative-fixtures
```

Pick an `--out` **outside the repository** — the script refuses any path inside it, because
3 GB of generated media has no business in git. It prints an up-front ETA and size
estimate from measured encode rates, skips any shape whose output already exists and still
ffprobes correct (encoding is the expensive part; `--force` overrides), and cleans up its
own intermediates.

Output layout — **movies and shows must be separate Plex libraries**, the scanners assume
the two content types live apart and a mixed root matches badly or not at all:

```
~/plxnative-fixtures/
  fixtures.json
  Movies/
    PlxTest H264 AC3 1080p (2001)/PlxTest H264 AC3 1080p (2001).mkv
    PlxTest HEVC 4K HDR10 TrueHD (2002)/…
    …
  TV Shows/
    PlxTest HDR Show (2010)/Season 01/PlxTest HDR Show (2010) - s01e01.mkv
    PlxTest HDR Show (2010)/Season 01/PlxTest HDR Show (2010) - s01e02.mkv
    PlxTest SDR Show (2011)/Season 01/…
```

## 3. Add two libraries in Plex Web

**First: the PMS process has to be able to READ the output path, as its own user.** The
default `--out` is under `$HOME` on the machine you generated on. If your PMS runs on
another box, in Docker, or as the `plex` user against a mode-700 home directory, the library
adds cleanly and scans **zero items** — which reads exactly like "the generator produced
nothing Plex likes". Generate into a path the server already serves, or move the trees
there.

*Settings → Manage → Libraries → Add Library*, twice:

| | type | folder | **agent** |
|---|---|---|---|
| 1 | Movies | `~/plxnative-fixtures/Movies` | **Personal Media** (a.k.a. *Other Videos*) |
| 2 | TV Shows | `~/plxnative-fixtures/TV Shows` | **Personal Media Shows** |

The agent matters. These clips match nothing online. With the normal Plex Movie agent the
items still scan, still get a ratingKey and still play — they just sit there unmatched with
wrong-looking posters, and a stray match onto a real film would be worse than none.

Then **wait for the scan and the analysis pass to finish** (*Settings → Manage →
Console/Activity*). A ratingKey exists the moment the file is scanned, but the *stream
list* the harness's assertions read comes from the deep analysis, and querying too early
gives you an item with a media part and no streams on it.

**Share both new libraries with the managed test user.** `tests/run.py` plays as the
`test_user` in your overlay, not as the owner — that is what keeps test playback off your
real watch history. A library the owner can see perfectly in Plex Web is invisible to that
user until you add it in *Settings → Users & Sharing → ‹user› → Libraries*. Miss this and
**all 21 cases fail at once**, against media you are looking at on screen. It is the
worst-shaped failure in this whole walkthrough.

**If you regenerate at a different length, Refresh Metadata on the item afterwards.** A
`--quick` set and a full set share the same paths by design, and PMS keeps the duration it
last analysed. Point the harness at an item Plex still believes is 20 s long and
`subtitle_text_srt` seeds a 843 s resume into it; the case fails as a player bug.

## 4. Map the keys into `tests/manifest.local.json`

`fixtures.json` is keyed by shape, so the mapping is mechanical. To find a ratingKey: open
the item in Plex Web → **⋯ → Get Info → View XML**; the URL that opens ends
`/library/metadata/<ratingKey>?…`. Or list a whole section at once:

```sh
curl -s "http://<pms-host>:32400/library/sections/<section-id>/all?X-Plex-Token=<token>" \
  | tr '<' '\n' | grep -E 'ratingKey=' | head -40
```

The token is the one in the gitignored `src/config.local.h`. **Do not paste it into
anything tracked**, and note that this repo is public.

Copy `tests/manifest.local.json.example` to `tests/manifest.local.json` (gitignored) and
fill `items` with the ratingKeys. The shape keys are identical on both sides:

```json
"items": {
  "movie_h264_ac3_1080p":            "12345",
  "episode_hevc_4k_hdr10_eac3":      "12346",
  "episode_hevc_4k_hdr10_eac3_next": "12347",
  "movie_hevc_4k_hdr10_truehd":      "12348",
  "movie_hevc_4k_dovi_p8":           "12349",
  "movie_hevc_aac_mp4":              "12350",
  "episode_h264_aac":                "12351",
  "movie_h264_ac3_many_audio":       "12352",
  "movie_av1_no_dp_audio":           "12353",
  "movie_hevc_4k_pgs_subs":          "12354",

  "movie_in_home_catalog":           "12345",
  "movie_cast0_in_both_libraries":   "<ratingKey>"
}
```

The last two are **fps-scene keys, not playback shapes**, and they are in the example
overlay too — leaving them out of this block is how `fps:detail-transition` and
`fps:home-detail-nav` get silently dropped for no reason. `movie_in_home_catalog` is
satisfied by **any** of the movies above once it appears in Home's recently-added row, so
reuse a ratingKey you already have. `movie_cast0_in_both_libraries` is the one key this
generator cannot satisfy at all — Personal Media items carry no cast — so leave it bracketed
and `fps:person-page` skips by name.

`episode_hevc_4k_hdr10_eac3_next` must be the episode that *follows* the one above it in
the same season — s01e02 of `PlxTest HDR Show`, which is what the generator built it as.

Leave any shape you did not build **bracketed**: the cases that need it are then skipped by
name with the reason printed, and the rest of the matrix runs.

## 5. Run the suite

```sh
./tests/run.py --list          # what will run, and what will skip and why
./tests/run.py
```

Everything else — the TV address, the flavour, the test user — is described in
`tests/README.md`. Two things about `--fps` DO change because the media is synthetic:

- **`fps:search-type` and `fps:search-idle` seed the literal query `th`,** and no title this
  generator writes contains it. `search-type` then has no result shelves for its oscillator
  to sweep and fails its `fps_floor`; `search-idle` passes its `fps_ceiling` for entirely the
  wrong reason. One false failure and one false pass. Change the seed in your overlay to a
  string your fixture titles actually match (`Plx` does).
- **The fill-rate scenes grade a lighter screen than the one they were baselined on.**
  Personal Media items get a frame grab at best and no backdrop art, and the floors in
  `tests/manifest.json` were measured against a real library with posters, backdrops and
  image decode on every card. `home-grid`, `home-hero`, `library-scroll`, `library-switch`,
  `item-menu`, `detail-transition`, `home-detail-nav` and `home-acct-glass` will pass here —
  they are not gating what their notes say they gate. Treat the fps tier on a fixture library
  as reported, not as a regression gate.

---

## The shapes, and why each is the size it is

Every duration is the deepest depth that shape's cases reach — `setup.viewOffset_ms`, seek
`target_s`/`final_s`, resume `offset_s`, `min_pos_after_s` + `min_climb_after_s`,
`expect.min_timeline_climb_s` — **divided by 0.9**, because Plex marks an item watched past
~90% and then drops the `viewOffset` the harness just seeded. Get that wrong and
`subtitle_text_srt` silently becomes a play-from-zero test that passes. Nothing is ever
shorter than **1.5× the longest `run_secs` of any case that names it**: an item that hits EOF
inside its `run_secs` fires the finish → Up Next → auto-advance chain, contaminating
`no_playing_error` and the teardown of a case that was only ever about playing. (That rule
used to be a flat 60 s floor, which gave four shapes *zero* margin against cases capped at
exactly `run_secs: 60`. Invisible on the passing path — the harness ends a case the moment
its assertions pass — and on a failing one it stacks a second, spurious failure on top of
the real one.)

| shape | dur | what makes it this shape |
|---|---|---|
| `movie_h264_ac3_1080p` | 1080 s | H.264 High + AC-3 5.1 + **four** SRT tracks ordered `[rus-forced, rus, eng, eng-SDH]`. 1080 s because `subtitle_text_srt` seeds 843 s (843/0.9 = 937 floor). CRF 20, not the 25 the rest of the H.264 set uses, and it declares a **`min_mbit` floor `verify()` asserts**: both rapid-seek cases' H.264 half lives here and seek *coalescing* is what they grade. |
| `episode_hevc_4k_hdr10_eac3` (+`_next`) | 300 s ×2 | HEVC Main10 4K HDR10 (real mastering-display + CLL in-band) with a **German default** E-AC-3 at index 0 and English at index 1 — `audio_switch_native` picks row 0 and expects a native switch. Built as a pair for `marker_credits_up_next`, with a shared 130 s intro and a 40 s black credits tail (see *markers*, below). Also carries a `min_mbit` floor. |
| `movie_hevc_4k_hdr10_truehd` | 90 s | TrueHD 5.1 default + AC-3 5.1 sibling: the smart-direct-play shape. Also the item the two player-tier fps scenes use. |
| `movie_hevc_4k_dovi_p8` | 90 s | Dolby Vision **8.1**, RPU authored from scratch by `dovi_tool generate` and injected into a synthetic HDR10 base layer, muxed by `mkvmerge`. ffprobe reads back `dv_profile=8, bl_compat=1, el_present=0, rpu_present=1`, plus in-band `Dolby Vision RPU Data`. Carries the **four-track SRT stack** as well — its case covers `embedded-srt-many`. |
| `movie_hevc_aac_mp4` | 90 s | HEVC/AAC in mp4 (`hvc1`, faststart) with a **sidecar** `.en.srt`. AAC **stereo**: this case is the mov-demuxer/ADTS path, real-world mp4s are usually 2.0, and `devcaps::audio_has` ignores the channel count so a 5.1 claim is one nothing in the app would ever check. |
| `episode_h264_aac` | 90 s | H.264/AAC episode with no subtitle tracks at all. |
| `movie_h264_ac3_many_audio` | 90 s | **Eight** audio tracks, each language-tagged, with **DTS English at index 6** — `audio_switch_transcode` picks row 6 and expects the switch to force a transcode. |
| `movie_av1_no_dp_audio` | 780 s | AV1 4K + Opus: no direct-playable video *and* no direct-playable audio, so the server must transcode. 780 s because `resume_transcode` seeds 600 s. **Its three cases need a Plex Pass server** — see below. |
| `movie_hevc_4k_pgs_subs` | 780 s | HEVC 4K HDR10 + a **PGS bitmap** subtitle track at index 0 (`subtitle_image_pgs` picks row 1; row 0 is *Off*). Seeds 600 s. |

**Track ORDER is part of the spec, not a detail.** `tests/README.md`: the audio tab's row is
the metadata audio index in file order, and the subtitle tab's row 0 is *Off* with row *r*
being subtitle index *r−1*. Three cases assert positions, and the generator's declarative
spec — the same one `verify()` reads back — is what pins them.

### You can see the shape without opening a manifest

Every clip has its stream layout **burned into the picture**, top-centre: the shape key, the
video line (which names Dolby Vision where it applies — otherwise the DV clip's burn-in is
byte-identical to the two HDR10 clips'), every audio track with its
codec/layout/language/default flag, every subtitle track. On top of that `testsrc2` draws
its own running timecode and frame counter in the top-left corner. So a media-source mix-up,
a wrong resume point or a seek that did not land is visible in a screen capture with nothing
to cross-reference.

**Top-centre, not bottom-centre.** At the bottom the plate lands exactly where the app draws
subtitles and where the PGS cues are authored, so on the two shapes whose whole case is
"is a subtitle on the screen" the two texts overprinted each other.

The same text goes into the container's `title`/`description` tags and into `fixtures.json`
— and `verify()` **reads the description back out of the file** and compares it with the
burn-in, so "the layout in the JSON" is a measurement rather than a restatement of the spec.
(A Personal Media library will not always surface it as the item summary, so paste it there
yourself if you want it in the UI.)

And **every audio channel gets its own pitch** — six distinct tones in a 5.1 track, LFE two
octaves down — so a bad downmix or a swapped channel map is audible rather than merely
plausible. The `join` filter is given an explicit `map=`: without it, `join` fills the output
layout in its own order (every mono sine's own layout is `FC`, so input 0 lands on *centre*)
and the whole channel map comes out rotated — six distinct pitches either way, which is
exactly why nothing looked wrong until somebody measured the channels.

---

## What this does NOT solve

**Intro/credits markers — the three `marker_*` cases.** Plex's marker detection needs Plex
Pass, ignores intros shorter than 20 s, and will not detect an intro ending past the
halfway point. `marker_intro_press` asserts `min_pos_after_s: 100`, so the intro has to end
at ≥ 100 s *and* before the midpoint.

The generator does its half, and **the modality matters more than the timing**: Plex
fingerprints **audio** and looks for the stretch that matches between episodes of a season.
So the episode pair now shares a 130 s intro whose audio is a three-tone melody identical in
both episodes, switches to a **per-episode pitch** for the body (the match therefore *stops*
at 130 s instead of running the whole episode and past the midpoint, where Plex discards the
candidate), and the burned-in layout banner — whose first line reads `S01E01` vs `S01E02` —
is **not drawn during the intro window** at all, so the intro is frame-identical too. For
`marker_credits_up_next` there is a 40 s tail of black picture under a distinct low audio
bed, which is what Plex derives a `final` credits marker from; before that, nothing on disk
addressed that case at all (testsrc2 runs full-brightness colour bars to the last frame).

**Three switches have to be on, and two of them are not on by default.** Before concluding
the detector does not fire on synthetic content, check all three: *Settings → Manage →
Library → **Generate Intro Video Markers*** at the server level, whose default is *"As a
scheduled task"* — meaning it runs in the maintenance window, **not** on scan; the
per-library *Advanced → Enable intro detection*; and, separately again, the credits
equivalent for `marker_credits_up_next`. A newcomer who scans, sees no markers and stops has
most likely met a scheduler, not a limitation. [Plex's own docs, not measured here.]

The intro boundary is authored at **130 s of 300**, deliberately mid-window rather than at
the 105 s an earlier draft used. Both satisfy "≥ 100 s and before the midpoint" on paper,
but Plex matches fingerprints and is free to trim the match: land at 98 and
`marker_intro_press` fails while every visible piece of evidence points at the app's
skip-intro latch instead. There is nothing to buy with that margin.

All four properties are asserted out of the finished files — intro audio identical, body
audio different, intro picture identical, credits tail black and on a different bed — and
recorded under `markers` in `fixtures.json`. What is **not** asserted, and cannot be from
here, is whether Plex's analyser actually *fires* on synthetic content: it has not been
confirmed on a real server. Treat the three marker cases as a real-library concern until
somebody demonstrates otherwise. `--no-markers` (formerly `--no-intro`, still accepted)
skips both splices; `--quick` never builds them, since a 20 s clip cannot carry a 130 s
intro.

**The three AV1 cases need a Plex Pass server with HEVC encoding enabled.**
`transcode_av1_no_dp_audio`, `seek_transcode` and `resume_transcode` all assert
`codec: hevc`, and `transcoder.rs` sends `videoCodec=hevc,h264` precisely because HEVC
*encoding* sits behind Plex Pass — a free server picks h264 and those three cases go red
reading `codec=h264 … (want hevc)` while the media is perfectly fine. Enable *Settings →
Transcoder → Enable HEVC video encoding*, or bracket `movie_av1_no_dp_audio` in
`manifest.local.json` and let the three skip by name.

**`movie_cast0_in_both_libraries`** — one of the two fps-scene item keys. It needs a *matched*
item whose first-billed cast member has titles in **both** libraries, so the person page
draws two poster shelves. Personal Media items have no cast at all, so this is
unreachable by construction; the `person-page` scene stays a real-library scene.
(`movie_in_home_catalog`, the other fps-only key, is satisfied in practice by any of these
movies — a freshly scanned library puts them all in recently-added — but that is a property
of the Home catalog, not something this script verifies.)

**`library_gaps`.** The synthetic-clip supplement `tests/manifest.json` describes (VP9,
MPEG-2, MPEG-4-ASP, 8-bit HEVC, 4K H.264, interlaced, FLAC/PCM/MP3, ASS, VobSub, mov_text,
HLG, HDR10+) is *not* built here. Those are fed through the `plxnative-url` boot trigger
rather than through Plex, so they are a different tool. Four notes for whoever writes it,
each measured on this ffmpeg 8.1.2:

* **HLG needs nothing special beyond `setparams`.** An earlier version of this file said
  ffmpeg's Matroska muxer drops `arib-std-b67` and that HLG therefore had to be muxed with
  `mkvmerge`. It does not: `mkvinfo` shows the container's `Colour transfer: 18` and
  ffprobe reads back `arib-std-b67`. What loses the transfer is writing `-color_trc` as an
  *output* option with no `setparams` — and that form loses `color_primaries` too, which is
  the tell. It is trap 2, not the muxer. Do not send anyone to install mkvtoolnix for this.
* **`hdr10plus_tool` has no `generate` subcommand** (1.x: extract/inject/remove/plot/editor),
  so HDR10+ needs a donor file and cannot be synthesized from nothing.
* **VobSub is one command away from the PGS output here.** ffmpeg has no PGS *encoder* and
  refuses text→bitmap, but it does ship `dvdsub`, and bitmap→bitmap conversion is explicitly
  allowed: `ffmpeg -i <the PGS mkv> -map 0:v -map 0:s:0 -c:v copy -c:s dvdsub out.mkv`
  yields a `dvd_subtitle` track whose rects decode back out. That closes the one image-sub
  gap besides PGS without another tool.
* **VC-1 has no free encoder** and nothing here changes that.

**PGS variety.** The built-in writer authors a conformant-enough stream for ffmpeg's decoder
— single window, single object, one palette, epoch-start per cue, a 5×7 bitmap font on a
1920×1080 authoring canvas. It exercises the decoder and the client-side bitmap render
path. It does not exercise the *variety* a real Blu-ray rip carries (multiple windows,
cropping, palette updates, forced-subtitle epochs), and no synthetic writer of this size
will.

**VC-1 and Dolby Vision profile 5 / profile 7.** Not built, not buildable. See the caveat at
the top.

---

## Verified where, exactly — and what is still only reasoned

Everything above about the *files* was measured on the finished media with ffprobe/ffmpeg on
a Mac. Four things were not, and saying which is which is the point of this section.

* **Nothing here has been through a real Plex scan.** Steps 3–5 (agent choice, scan and
  analysis timing, ratingKey mapping) are reasoned from `docs/pms-api.md` and prior art, not
  executed. In particular: whether PMS enumerates a nine-stream file in **container index
  order** is what the `audio row 6 = English DTS` and `subtitle row 3 = English` contracts
  rest on. Everything client-side checks out; the server half is untested.
* **Whether the rapid-seek cases actually coalesce** at the bitrate these files carry
  depends on the LAN round trip and the television. The `min_mbit` floor puts the H.264 seek
  shape at roughly the measured bitrate of real Creative-Commons footage, which is the best
  a host-side check can do.
* **The HDR shapes are SDR-valued pictures *relabelled* PQ.** `setparams` stamps
  bt2020/smpte2084/bt2020nc onto colour bars that were never tone-mapped, which is correct
  for every assertion the suite makes — Plex labels HDR from the transfer function and the
  app reads the same — but a human looking at one on a real HDR panel will see a wildly
  wrong-looking picture. That is the fixture, not the app.
* **Mastering-display and MaxCLL live in the HEVC SEI, not in the Matroska container.**
  `mkvinfo` shows the `Colour` element carrying transfer/matrix/primaries/range and **no**
  `MasteringMetadata` or MaxCLL. That is right for this app (`ff.rs` keeps the 137/144 SEI
  in-band deliberately, and nothing in the tree reads a container-level copy), and
  `verify()` asserts the in-band form by decoding a frame. If a container copy is ever
  wanted, `mkvmerge --chromaticity-coordinates / --white-colour-coordinates /
  --max-luminance / --min-luminance / --max-content-light / --max-frame-light` writes it.

One known blind spot shared by `verify()` and the on-device assertion, recorded so it is not
rediscovered as a surprise: a PGS palette with **alpha 0** would still decode as
`num_rects=1` with a non-zero rect, so both would pass on a subtitle nobody can see. The
built palette is 255/160, so this is not the case today.

---

## Flags

| flag | |
|---|---|
| `--out DIR` | output root; default `$FIXTURES_OUT`, else `~/plxnative-fixtures` — the same variable the Makefile and `tests/run.py` read, so all three agree without a flag. Refuses any path inside the repo. With `--tier pipeline` the pack goes in a `pipeline/` subdirectory, and naming that subdirectory yourself is idempotent (both spellings of the seam land in one place). |
| `--tier T` | `integration` (default) or `pipeline`. **Two packs, for two suites.** The default builds the media the 21-case on-device matrix names — full length, laid out in two Plex-scannable trees, because every duration in it is a *Plex* constant (the ~90 % watched threshold that drops a seeded resume point, the marker windows, the Up Next tail). `pipeline` builds eight short clips, flat, for `./tests/run.py` — the DEFAULT tier since 2026-08-22, which serves them off this machine over HTTP and plays them with no Plex anywhere: ~0.3 GB and ~1.5 min against ~3 GB and ~20. |
| `--secs N` | override every shape's duration. Generalises `--quick` across its whole range. Note what the harness does with a pack built short: a pipeline case that seeks deeper than the clip is **skipped with the reason named**, not failed — the failure it would otherwise produce reads exactly like a player regression. |
| `--quick` | every shape at ~20 s, whole run ~75 s. **Development only** — structurally correct but shallower than every seek, resume and marker depth the suite asserts, and short enough to hit EOF inside a case. The script says so on every quick run, and every record it writes is stamped `quick: true`. |
| `--only K[,K]` | build a subset; comma-separated **and/or repeated**. Accepts `episode_hevc_4k_hdr10_eac3_next` and maps it to its pair. |
| `--list` | shapes, durations, output paths. |
| `--force` | rebuild even when the existing output verifies — and the only way to *shorten* a full-length file with `--quick`. |
| `--no-markers` | skip the shared intro segment and the black credits tail in the episode pair. `--no-intro` is accepted as the old spelling. |
| `--keep-work` | keep `<out>/.work` (banners, SRT/SUP assets, DV intermediates). |

Exit code is nonzero if any shape failed to build, failed its own verification, or failed
the cross-episode marker check. Missing optional tooling is **not** a failure at any count,
including when it skips everything.

**Mixing `--quick` and full-length runs in one output root is safe but not suite-valid.** A
`--quick` run will not shorten a file that is already longer than it asked for (it prints
`longer than asked — kept`), each record carries its own `quick` flag, and the document's
top-level `quick` reads `"mixed"` when the set is half and half. That flag is the one
machine-readable "is this set usable by the harness" marker, so it is derived from the files
rather than stamped from the invocation — it used to say `false` over a set that was still
20 s clips.

---

## For maintainers

**Self-verification is the load-bearing part.** After each build, ffprobe reads back
everything the shape claims — duration, **overall bitrate against a `min_mbit` floor where
one is declared**, the container description against the burned-in layout, codec, resolution,
pixel format, codec tag (`hvc1` vs `hev1`), the video default flag, colour
primaries/transfer/matrix, in-band mastering-display and content-light SEI, the DV
configuration record's profile / base-layer compatibility / **el_present / bl_present /
rpu_present** plus the in-band **RPU NALs**, the audio track list *in order* with channels,
language, title and default flags, the subtitle list with codec/language/title/forced, **how
many cues each subtitle track carries and how late the last one is**, decoded PGS display
sets, and sidecar presence. **A shape that fails its own check is a hard error naming both
sides**, and the file is not recorded in `fixtures.json`. This is not ceremony: a generator
that silently emits the wrong shape is worse than no generator, because the harness then
fails as if the *player* regressed and every piece of evidence points at the app.

**A check that cannot fail is a lie in the output**, so the ones added last are worth naming
— each was found by building a file that *should* have failed and did not. A stunted SRT
with one cue at t=0 kept its codec, language and forced flag and verified clean, while
`subtitle_text_srt` (which seeds 843 s) failed on the television as `no sub cue`, i.e. as a
demuxer regression. The same for PGS and its 600 s seed. A DV file with `el_present_flag=1`
verified clean while `route.rs` would refuse to direct-play it. A `hev1` copy of the mp4
verified clean. A re-encode with **no burn-in at all** verified clean, and `fixtures.json`
still recorded the layout, because that field came from the spec and was never read back.

**The pair check is separate.** `verify_markers` grades the intro/credits segments across
*both* episodes — a property one file cannot express, and the one this generator originally
got backwards (differing in video, identical in audio; Plex reads audio). A marker failure
does not drop the records: the files are still correct fixtures for the shape's other cases.

**`fixtures.json` is re-verified end to end on every run, including the records the run did
not rebuild.** The document accumulates across invocations — a `--only` run, or a run on a
machine without `dovi_tool`, leaves every other key in place — and those keys used to be
copied through verbatim under a fresh `generated_at`, never re-checked, not even tested for
the file still existing. Since it is the document a resolver maps into
`tests/manifest.local.json`, a carried-forward record pointing at a replaced file maps a
shape key onto media that is not that shape. Now every untouched key is re-verified at *its
own* length and dropped (with the reason printed) if it no longer holds, and the shapes
skipped for missing tooling are listed under `skipped`.

**When a case moves, move the spec.** If a case changes its `viewOffset_ms`, its seek target
or a menu row, the shape's entry in `SHAPES` is the one place to update — durations, track
order and the burn-in text all derive from it, and `verify()` reads the same dict. The
module docstring carries the full trap list the code is built around (ten of them, each one
a silent failure), including why `-t` goes before `-i`, why `setparams` must follow
`format=`, why `testsrc2` and not `testsrc`, and why `-preset ultrafast` is wrong for H.264
and right for HEVC.

**The rate/size table is measured, not guessed.** `SHAPES[k]["rate"]` (encode seconds per
output second) and `MBIT[k]` exist only to print an honest ETA. Re-measure both if the CRFs
move; a wrong ETA on a twenty-minute job is how somebody concludes it hung.
