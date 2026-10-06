# Drill-in sub-menus in the player track-menu popover

Design record for the multi-page Subtitles / Audio popover (Other languages, per-language pages,
Style pickers) and the table, form and clip primitives it stands on. Planned with an independent
model reviewer over several rounds; this file keeps the decisions and their reasons.

**Status:** PR 1 (table groundwork), the track menu on keyed forms (Settings form migration 5/5,
#339: `appkit::track_menu::TrackRow`) and PR 2 (the page stack in the Subtitles tab on that form: Style,
the Size / Position / Color pickers, nav keys, persistence, locks, the rebuild signature and replay
state) and PR 3 (Other languages, the language pages, the image-subtitle badge) and PR 4 (the animated
panel resize and page slide for Tracks and More, `ui::panel_motion`, plus the device frame-time
scene) and PR 5 (More -> Quality on the shared `ui::page_stack::PageStack`, the `more-quality-osc`
scene) are what this repository has. The replay anchors were re-recorded for PR 2 and again for PR 5
because the overlay's state shape changed on purpose.

## Behaviour

- **Subtitles root:** a Subtitles section (Off plus single-track languages), one section per
  multi-track language of `yours`, an "Other languages N" row, then Timing (a value row, no
  chevron; it hands off to the timing capsule) and Style. **Other languages:** one row per
  language A-Z; a single-track language is a direct pick row, a multi-track one is a "French, 3
  tracks" drill-in. **Language page:** Full / SDH / Forced / Commentary ranked, identical ones
  numbered. **Style:** Size, Position and Color drill into picker pages with checkmarks. When
  the host can run a subtitle search, the root ends with a **Search subtitles** row: its page
  has a Language row (defaulted to the viewer's subtitle preference; a picker of 2-letter
  languages overrides it) and the agent's results; OK on a result downloads it, and once the
  server lists the new stream the panel selects it and closes (`docs/pms-api.md` §8).
- **Keys:** UP/DOWN move; OK or RIGHT on a Nav row pushes; on a sub-page LEFT/BACK pops and focus
  returns to the opener by semantic id; on the root LEFT/RIGHT switch tabs as before (RIGHT on a
  Nav row pushes instead); BACK on the root dismisses; clicking the "< TITLE" band pops. A track
  pick at any depth commits and dismisses; a Style pick commits and stays. The menu reopens at the
  root.
- **Format badge (owner edit, 2026-10-01):** only image subtitles (PGS/VobSub) show one, a chip with the format's short name ("PGS", "VOBSUB", "DVB" for Plex's `hdmv_pgs_subtitle`, `dvd_subtitle`, `dvb_subtitle`; `sub_layout::image_codec_badge`) that FOLLOWS the row's kind chip (SDH, Forced, External) instead of being outranked by it, so an SDH bitmap track still says what it is; text subtitles show no format, and the design board's "SRT" sub-lines are gone. A kind chip that only repeats the row's own label ("SDH" on the row called SDH) is still dropped; the format never is.
- **Other languages row:** one Nav row on the root reading the language count (no "N languages" header accessory); when the active track lives behind it (a live change put it there) it reads a check and the track's language instead. On the page, a multi-track language that holds the active track reads a check and that variant ("SDH"), otherwise its track count.
- **Page identity:** a language page is `TrackPage::Language(stream)`, its opener `TrackRow::OpenLang(LangId)`, where `stream` is the Plex stream id of the language's first track in the item's FULL list (`sub_layout::LangId`), never a list position: a track leaving mid-play shifts every index, and an index-keyed page would silently relabel to another language. `LangId` also carries a slot, the language's ordinal on the Other languages page, which only decides the row's focus key (re-derived on each build, so the key may move when a language arrives above it; equality ignores the slot). The page's data is `TrackMenuState::other`, refreshed from the same `sub_model` the root is built from; a page whose language is no longer listed (its first track's stream is gone) pops to the root.
- **A language changing shape (one track <-> several):** on the Other languages page a single-track language is a direct pick row and a multi-track one a drill-in, so a language that grows or shrinks while the viewer is on its row gets the focus carried to its new row (`other_row_for`). A language page whose language drops to ONE track stays open and lists that track (a page lists whatever its language still offers; popping is the only shape change), and the pop lands on the language's now-direct row on Other languages.
- **Harness `track:N`:** the N-th track in page order: the root's track rows, then the tracks behind Other languages A-Z, a multi-track language expanded into its ranked page. It commits by the track's own list index (`commit_sub_track`), not by focusing a row, so it reaches a track whose row is on a page that is not showing.
- **Title band:** a larger gap under the "< TITLE" caption (`table::TITLE_GAP`) before its hairline.
- **Why the root row shows the language and the variant is shown on the page:** when the active track lives behind Other languages, the root's row reads a check and that track's LANGUAGE ("French"), not the variant: a stacked variant on a row would read as a description of the row, and the variant ("SDH") is already answered one level down, where the language's drill-in on the page reads the check and the variant and its own page checks the track. An active track is behind that row in two ways. Normally its language is "yours" and it sits on the root, so the row reads the language count. But a CODELESS subtitle (no language code, grouped under "Unknown") and a track whose code names no language `lang_key` recognises are never added to "yours" (`overlay::subtitle_yours_langs` only adds codes `lang_key` accepts), so such a track opens behind Other languages even though it is the active one, and a live change can put any active track there too. In every such case the menu opens, and the live poll falls back, with focus on the Other languages row (the root's landing id is `TrackRow::OpenOther`), never on Off. `yours` stays the opening snapshot so groups do not reshuffle under the viewer.

## Availability and locks

- Style follows Timing's availability: omitted while a transcode has no subtitle on (nothing to
  style); once the server draws the selected subtitle (`route::subtitle_presenter` = `ServerBurn`) both
  stay, dim and inert, with a reason under Style: "Drawn by your server while it converts the audio."
  (a remux or the app's own burn) or "Drawn by your server at this quality." (any other burn).
- Per active renderer: image -> Size and Position dim and inert ("Image subtitles keep their own
  size and position."); native ASS/SSA -> the same ("Styled subtitles keep ..."); Color stays live in
  both, since the subtitle ink tints bitmaps and ASS.
- A dim row is focusable so the viewer can read why, and it is INERT at the form layer
  (`FormTable::activate` answers `None` for OK and RIGHT alike), not by a guard in each caller.

## Persistence and live preview

One optimistic operation, no deferred publication: `route::select_subtitle_size/position(v)` on the
main thread stores the live atomic, invalidates, and submits a persist-only closure that writes the
session store and never touches the atomic (mirrors `player::set_subtitle_tone`). Settings'
Size/Position arms use the same call, so no completion can overwrite a newer live pick; durable
writes are FIFO on the worker, last submitted wins. A failed write is logged, never alerted over
video, and claims nothing about the next boot. The live value survives BACK and dismissal (global
state, not panel state).

## Focus identity

Each page declares rows with semantic ids, one alphabet for every page: `appkit::track_menu::TrackRow`
(`Audio(i)`, `Boost`, `Loudness`, `SubOff`, `Sub(i)`, `Timing`, `Style`, `OpenField(field)`,
`Choice(field, rung)`, `OpenOther`, `OpenLang(lang)`). Selection and saved openers
are stored by id. `RowKey` is a hand-assigned family base plus a STABLE ordinal (the track's index
in the item's list, never a list position): `0x0001_0000` Audio, `0x0002_0000` the DSP pair,
`0x0003_0000` Off, `0x0004_0000` tracks, `0x0005_0000` Timing/Style, `0x0006_0000` the Style
page's drill-ins, `0x0007_0000` picker rungs (one 256-wide block per field), `0x0008_0000` the Other languages
drill-in and `0x0009_0000 +` a language's drill-in, which uses its slot on the Other page instead of
a track index. `OpenLang` carries the language's `LangId`, the stream id of its first track in the
FULL item list, not the currently offered members. Initial focus
per page is an explicit id (root: active track, the Other languages row when the active track is
behind it, or Off; language page: active variant else first;
Other languages: the checked row else first; picker: the checked choice; Style: Size);
`opening_row` is only the fallback. LEFT/RIGHT stay on `EdgeRule::Screen` -> edge key; there is no
second ladder.

## Replay state

`PlayerOverlayArg` stays the opening address. The screen canonicalises tab, page path, selected
`RowKey` and each stacked page's saved return id; the SHAPE string changes and anchors are
re-recorded.

## Code structure

- **Table operations** (`ui/form.rs` `FormTable`, over `TableView`): `open` (snap, scroll 0,
  explicit initial id), `refresh` (same page, data changed: keep scroll and pill, restore by id, the
  pill slides), `refresh_with` (the same with a banked `prefer` id and a `fallback` id: prefer, then
  the id the table was on, then fallback, then the old-order neighbour) and `restore` (a pop: reinstate saved scroll and selection). `set` stays Settings'
  snap-and-reset. They differ because "the page changed", "its data changed" and "a page came
  back" want different scroll and pill behaviour.
- **Page stack** (`TrackMenuState::pages`, `TrackPage`): one `FormTable` serves the active page;
  a push saves the opener's `TrackRow` and the scroll, a pop restores both. To add a page: a
  `TrackPage` variant (with a `title` and a stable `code`), its form in `page_form`, its explicit
  `page_initial`, a Nav row whose `Dest` is the variant, and the `TrackRow`s it needs.
- **Form extensions**, reusable by Settings PRs 3-5 (`docs/settings-form.md`): a Nav row gets the
  chevron from its kind; Choice rows derive `checked` from a current-value predicate; an item can be
  disabled (dim, focusable, inert).
- **Title band** (`TableView::set_title`): "< TITLE" in the section-header caps caption, then a
  `DIV_H` divider gap, so it reads as the page's heading and not a caption glued to row one. The
  glyph is separated from the text by the check-column->label gap. It is part of `measured_height`,
  `measured_width` and `fit_report` (`FitRole::Title`). Its click target is the owner's, not the
  table's: a pointer-only key that pops, never in the D-pad column.
- **Wrapped notes**: `Row::note` wraps at the label column; the line count is resolved against the
  frame `Measure` (`TableView::fit_notes`), never read stale, and `fit_report` judges notes.
- **Composable clipping**: `ClipScope` (`DrawFrame::clip`, or `ClipScope::open_in` for a widget that
  holds only a `Painter`) is the ONE scissor stack: a nested scope INTERSECTS the enclosing one and
  restores it on drop. `TableView::draw` uses it, so two table draws can sit inside an outer panel
  clip. Painting and hit clipping share the mechanism; there is no second stack.
- **Pointer during transitions**: a surface-level pointer hold the dispatcher consults before hover
  and activation; while held, pointer input is swallowed (never a miss, so never dismiss).
- **Animation** (last PR): the page transition is internal to the page stack; the modal stack keeps
  appear/dismiss/input phases. Each page's layout is computed once when its inputs change; the spring
  animates only the panel rect and the two content layers' x offset and alpha under the clip stack.
  One panel background, no backdrop capture or transition textures. Key presses act on the logical
  state immediately (a push during a push retargets; push then pop mid-slide reverses).
  Built as `ui::panel_motion::PanelMotion`, owned by `TrackMenuState` and `MoreMenuState`: the
  natural layout is cached against `TableView::layout_rev` (re-stamped by every content change) and
  only recomputed then; springs animate the panel's top and left edges (bottom and right stay
  anchored, so the card grows and shrinks away from the HUD); a push or pop hands the OUTGOING page's
  table to `begin_slide` (stored without its focus pill: only the arriving page draws one), and
  `draw` paints one background, opens `ClipScope::open_in` at the animated rect pulled in by
  `CLIP_INSET` (the scissor is rectangular and cannot follow the card's rounded corners), and draws
  the leaving page(s) and the live page on a STAGGERED fade so they never read on top of each other:
  a leaving page fades out in 0.14 s while it travels 22% of the card's width toward the exit side
  (push left, pop right), and the arriving page starts from transparent only once every leaving page
  is at or below `GATE` (0.1) and fades in over 0.22 s from the entry side. A layer below 1% alpha is
  not drawn. Every layer owns its alpha and offset, and a swap hands them to the layer's new role:
  an opposite-direction key (push then pop) revives the page that was arriving from its own alpha,
  and a push during a push turns the arriving page into a leaving one from where it is, so no key
  steps the picture. Audio <-> Subtitles resizes the
  card only; More resizes when its row set changes and also slides between its root and the
  Quality page. Only the live page registers focus stops, at
  the animated position (`place` agrees with `Part::draw`), and `Screen::pointer_held` makes the
  dispatcher record but never resolve pointer input for the slide plus the rect lag. A settled
  panel reports no motion to `ui::idle` and asks for no frames.

## Rebuild signature

A live poll rebuilds the current page when any of these change: the subs fingerprint (count, stream
ids, offered sidecars), the active index, the renderer kind (text / image / ASS), transcoding, or the
presenter (`route::subtitle_presenter`) / enhancement route and subtitle effect (what the Style rows' lock and Timing's omission read).
On the root the change refreshes in place. On a sub-page the page is refreshed in place too (focus kept by id) and pops to the root by id only when its availability no longer holds: the renderer kind changed, or Style's availability did (the server drawing the subtitle, or a transcode with no subtitle that omits it), or a page's own listing is gone (`TrackMenuState::pages_hold`: an Other languages page with no language left, a language page whose language, by stream id, is no longer in `other`).

## PR sequence

1. **Table groundwork:** wrapped notes with `Measure` at update, title band, composable clip,
   form.rs extensions and the three table operations; geometry and fit tests.
2. Keyed track forms (Audio + Subtitles), the page stack, Style pickers, persistence, locks, replay
   state, pointer and BACK behaviour.
3. Other languages, language pages, the badge change, the invalidation fingerprint.
4. Resize/slide animation for all table popovers (Tracks, More); the `track-menu-submenu-osc` scene.
5. More -> Quality: the root's headerless Quality row (value = the current rung) pushes a Quality page
   on `ui::page_stack::PageStack` (extracted from the track menu); live refresh keeps focus by id and
   pops to the root when Quality becomes unavailable; the failure screen's quality entry opens on the
   page, and BACK pops to the root before it dismisses; the `more-quality-osc` scene
   (`plxnative-more=1`, `plxnative-moreosc=<period_ms>`, `dev::scenarios::moreosc_arm`).

## Decisions

- **Minimum width (owner):** the in-player popovers (Tracks, More) have a floor, `theme::layout::PLAYER_MENU_MIN_W` (440 px at 1080p), set on their `TableView` (`min_panel_w`), so a small page does not shrink to its labels. A floor only: wider content still grows the panel to `MENU_MAX_W`. At 440 the locked-renderer note takes two lines in English and Belarusian and three in Spanish (two needs ~520).
- **Geometry while the Position picker is open (owner): accept the overlap.** Panel bottom is fixed;
  at High the caption may pass behind the panel. No preview shift, no page-dependent HUD policy.
- **Evidence for the animation PR:** host tests for running vs resting (a transition reports
  animating and stops requesting frames at rest; push then pop reverses and settles); on device a
  frame-time capture during repeated push/pop with cues uploading, plus a motion capture the owner
  watches; an idle ceiling only on a plane-unbound fixture, because a bound video plane forces
  presents.

## Device frame-time check

`track-menu-submenu-osc` (`tests/manifest.json`; trigger `plxnative-submenuosc=<period_ms>`,
`dev::scenarios::submenuosc_arm`) loops real keys through the Subtitles menu: Style, the Size
picker, back, back, Other languages and back (when the item has more than one subtitle language),
the Audio tab and back. It needs no Plex account. Its `worst_ceiling_ms` of 25 is a budget, and
the TV does not meet it yet. Measured on 2026-10-01 (screen on, 90 s legs, guest + mock), the grade
(the 2nd-highest post-warmup `worstframe=`) was 25.3–29.5 ms on most legs and 42.0 ms on one, at
59–61 fps. The legs were taken across commits `60bf990cc` → `929e28e9a`, not on one build; only
the final M_osc leg ran at `929e28e9a`. None of these numbers is a measurement of a later commit,
and the feed-slice low-water exemption and the background-drain occupancy bound (both later) have
not been measured on the TV. The menu-closed control on the same clip graded 25.5–27.2 ms. What is left over 20 ms:

- **Back-buffer waits.** Most of these frames spend 13–39 ms in `clear`, the frame's first
  framebuffer command, and draw almost nothing else. They come at the same rate with the menu
  closed, and they don't line up with Starfish feeds. Measured on 2026-10-01 (panel off, guest +
  mock, every frame logged with `--arm framedrop=0.01 --arm framecb`): the compositor's own
  `wl_surface.frame` stamps show its repaint arriving 20–36 ms after the previous one while our
  commit before it was on time, and it does so on Settings and Home with no video at all
  (`plxnative-noidle`), so it is not the player route and not the app's GPU load (a menu-closed
  player frame is a bare clear). 5–36 late repaints per ~95 s leg, varying leg to leg.
- **A late commit.** The rest of the frames over 20 ms follow an on-time repaint: the work after
  the clear (page, surfaces, swap) took 3–8 ms longer than usual, the commit moved by that much,
  and the next frame is correspondingly short. The transport's clocks were one cause, rasterised
  after the clear once a second (`textx2`, 1–3.5 ms); `player_hud::ClockWarm` now queues them from
  the page's `prepare` so they are rasterised before it. In every other such frame the frame
  thread's CPU time over the draw and the swap stayed flat while the wall time rose (the probe's
  `d=`/`s=` fields, wall/cpu/run-queue ms): the thread was off the CPU, not busy.
  `--arm framedrop=17 --arm framering --arm framecb` captures the same evidence while writing only
  the slow frames and their neighbours.
- **The open frame, ~27–34 ms.** `ulatch` (5.5–7.3 ms) is the modal underlay latching its field
  from the UltraBlur corners, and it was paid again on every open. `navcommit` is ~6 ms, of which
  `tmnew` is under 1 ms. Then come the root page's first strings. Since then the corner envelope is
  kept across closes and preloaded once the page has rested ~0.5 s with nothing open (`ModalUnderlay::retire`/`note_at_rest`/`preload`), and
  the overlay's `prepare` queues the root strings on the mount frame (`warm_open`): the open frame
  measured 17.8-21.7 ms on the 2026-10-02 capture (below), the rest being `after_step`'s focus seat.
- **Playback start.** The Play/ACB call and single Starfish `Feed()` calls block for 5–28 ms in
  the first seconds.

**Back-buffer waits: what the 2026-10-02 capture showed.** Build origin/main 6ee274c2c, `--guest`
and the mock server, the television's panel off and muted, the player with the track menu open and
the sub-menu oscillator running. Three legs, each on its own launch inside a `tv-lock` lease. The
tools below are the how-to; the findings follow.

1. **Kernel scheduler trace.** Boot with `--arm framedrop=17 --arm framering=17 --arm framecb`
   (the ring only exists while `framedrop` is armed). During the leg run
   `tools/tv-sched-trace.sh --secs 6 --tid <app pid> --out sched.gz`: it mounts tracefs if needed,
   records `sched_switch`, `sched_wakeup`, `irq_handler_entry/exit` (arch timer filtered out), any
   mali/kbase/gpu event category and, with `--tid`, that thread's raw syscalls, on
   `trace_clock=mono`, drains `trace_pipe` (bounded by `--read-timeout`, default 90 s; a stopped
   read-back leaves a valid partial gzip and exit status 3), and restores the set afterwards. On the
   television's 4.4.84 kernel (event tracing, no function tracer, no SCHEDSTATS) the string filter
   `name != "arch_timer"` is rejected and the tool falls back to the arch timer's irq numbers; the
   `raw_syscalls` filter `common_pid == N` is accepted; the per-CPU `stats` `entries:` line reaches
   0 when the buffer is drained; and a `trace_pipe` read-back of 433,702 events (9.3 MB gzipped)
   finished well inside the 90 s bound. The first run read the non-consuming `trace` file, which took
   minutes for under 4 s of data, and at 20 s the ring overran (1.3 M events written, 0.87 M kept),
   hence the 32 MB ring cap. An 8 s run at the cap still overran CPU 0 only, by
   5,644 of 166,017 events (CPU 0 carries about twice the events of the others), so `--secs 6` is
   the default and the loss-free choice. Then
   `tools/analyze-sched-trace.py APP.log sched.gz --tid <app pid>` (`APP.log` from
   `tools/tv-session.sh log FRAMEDROP`; the frame thread is the main thread, tid == pid). For each
   slow frame it lists where the thread left the CPU (preempted or blocked, in which syscall when
   the trace has syscall events, who ran instead, who woke it) and where the vsync (`osd_irq`) and
   GPU interrupts and surface-manager's threads fell in the wait and commit windows.
   `tools/analyze-sched-trace.py --self-test` runs on the host.
2. **Mali HWCNT.** Boot with `--arm hwcnt=frame.ui` and nothing else profiling; the `glFinish`
   brackets serialize the pipeline, so no `fps=` from this leg. Pull `plxnative-hwcnt.jsonl` from
   the app's runtime directory, then `tools/analyze-hwcnt.py FILE --phase frame.ui --discard 60`
   and `tools/analyze-hwcnt-wait.py FILE`. The latter buckets the phase's serialized wall time and
   prints the GPU's active cycles per bucket: cycles that grow with wall time mean GPU-bound,
   cycles that stay flat mean the GPU sat idle while the frame waited.
3. **Wayland protocol log.** Boot with `--arm wldebug`: it sets `WAYLAND_DEBUG=client` and the
   event log gets a `wldebug:` line carrying `offset_us` (realtime minus monotonic). Intended:
   libwayland-client prints every request and event with a `CLOCK_REALTIME` microsecond stamp to
   `plxnative-stderr.log`. There is no analyzer for it.

Findings:

1. **The GPU is not the bottleneck** (Mali HWCNT, 3,694 `frame.ui` frames). GPU_ACTIVE is 6.56 M
   cycles per frame at p50 and 6.85 M at p95, against the ~10.6 M a Home hero frame uses at
   59-60 fps. In `analyze-hwcnt-wait.py`'s 20-24 ms wall bucket (87 frames) GPU_ACTIVE p50 is
   6.68 M, the same as the 16-20 ms bucket (6.57 M, 3,197 frames): the GPU sat idle while the frame
   waited. The 28-32 ms bucket (111 frames, 8.6 M) is the sub-menu transition frames under the
   `glFinish` serialization, still under budget.
2. **How the pacing works** (kernel trace, 8 s, 433,702 events, with the frame thread's syscalls).
   `osd_irq` fires every 8.33 ms (926 of 928 gaps), twice per 60 Hz frame. The frame thread blocks
   in `poll` inside the first framebuffer command until surface-manager wakes it just after one of
   those interrupts. A normal frame then commits 3.2-4.0 ms after that interrupt and 4.3-5.1 ms
   before the next one. When a commit lands after that next interrupt, the following frame's wait
   is one full 16.7 ms cycle longer (23-24 ms instead of ~12.5 ms). Seen twice: seq 2906 committed
   0.58 ms after the interrupt (its draw+swap took 16.0 ms because the thread sat runnable on CPU 0
   for 12.6 ms while `n000002a`, a platform thread, ran 11.1 ms), and seq 2907 then waited 23.0 ms;
   seq 2947 committed 0.16 ms late (draw+swap 6.6 ms, preempted repeatedly by `tHDMISignal` and
   others), and seq 2948 waited 24.3 ms. So the app has about 8.3 ms from wake to commit, uses about
   3.5 ms, and has about 4.5 ms of slack. Work done before the first framebuffer command (ingest,
   results, nav commit, prepare) is absorbed by the ~12.5 ms wait and is not on that critical path
   until it exceeds it.
3. **Most long waits are not caused by the app.** In steady state (after the first 15 s) the two
   legs with the probe armed had 31 waits of 18 ms or more: 2 followed a late commit of ours (the
   preemption cases above) and 29 followed an on-time commit. Example: seq 3033 waited 21.5 ms
   after seq 3032 committed 3.45 ms after its interrupt with 4.88 ms to spare; the frame thread was
   blocked in `poll` with its CPU idle for 11.5 ms of that. In those frames `w=` shows one voluntary
   sleep, ~0.3-0.45 ms of CPU and ~0 run-queue wait. That is the compositor releasing the buffer a
   cycle late; nothing in the app's frame can change it. (Part of that count was taken while the
   trace was being read back, which loads the set.)
4. **Frame-thread preemption over the 8 s:** 4,922 preemptions while runnable, 419 ms in total
   (about 5% of the time), 8 over 1 ms, 2 over 3 ms (12.6 ms on CPU 0, 3.6 ms on CPU 3). Wake-up
   latency was never over 1.4 ms. The set runs at a load average of about 12 because every system
   process carries a `tLibSystrim` and a `tFragmentation` thread that wake roughly 100 times a
   second each (about 6.45 M wake-ups per thread over 18 h of uptime); they account for the most
   time run in place of the frame thread (49.6 ms and 33.8 ms of the 419 ms) but in slices of tens
   of microseconds. The frame thread spent 823 ms on CPU 0 and 755 ms on CPU 3 of its 1.9 s on-CPU;
   CPU 0 also carries the interrupt and audio/HDMI threads and had the only long preemption.
5. **What is the app's: the text prewarm drain had no per-frame bound.** `warmdrain` took 22.7 ms in
   one frame 1.8 s after playback start (frame total 34.9 ms), 15.8 and 11.9 ms in the first 0.2 s,
   and 9.8 ms on the menu-open frame at t0+6 s, where with the 8.3 ms nav commit it made the
   pre-clear work 19.6 ms and the frame 21.7 ms. The drain is now bounded by time (#376,
   `PREWARM_BUDGET_US`, live queue first). On the television the track menu's open frame went from
   `warmdrain` 9.8/6.6 ms (frame 21.7/17.8 ms) to 3.3/3.2 ms (frame 14.8/18.3 ms); one single-string
   18.9 ms drain at about 1.1 s after playback start remains.
6. **#372's preload.** The `upre` latch ran once, 1.4 s after the first frame in both legs, and cost
   9.6 and 11.2 ms in `prepare`; the frame totals were 16.5 and 17.9 ms. That is before the first
   framebuffer command, inside the wait, so it did not drop a frame, and the menu opened at t0+6 s
   with the field already held. The menu-open frame measured 17.8 ms in one leg and 21.7 ms in the
   other (the difference is the prewarm drain above).
7. **Leg 3 produced nothing.** `--arm wldebug` set `WAYLAND_DEBUG=client` (the `wldebug:` line is in
   the event log) but libwayland-client printed no protocol lines to `plxnative-stderr.log` on this
   firmware (webOS 4.10.2). The trigger is unverified as an instrument on this set.

The probe's run-queue field (`w=<wall>/<cpu>/<runq>`, from `/proc/thread-self/schedstat`) does
report non-zero values on this kernel (for example `w=16.13/0.38/3.96`), and
`/proc/<pid>/task/<tid>/schedstat` is populated.

Fixed on the way here:

- **Feed backlog.** The prime backlog used to be fed in one tick, and is now fed 3 ms per lane per
  tick (`FEED_LANE_SLICE_US`) once a lane holds its prime depth. A lane below low-water (priming,
  or under `FEED_LOW_WATER_NS` of lead) is exempt, so a slow `Feed()` cannot stretch the prime or
  drain the lead.
- **Cold first tab switch.** The first switch to the other tab rasterised its strings cold
  (`textx8:9.5`). They are now warmed in the background, in whatever is left of the
  frame's 2.5 ms prewarm budget after the live page (`TrackMenuState::warm_other_tab`;
  `PREWARM_BUDGET_US`, a time bound: the count it replaced, eight strings, was `warmdrain:22.7` on
  one frame).

`more-quality-osc` (trigger `plxnative-more=1` to open More, `plxnative-moreosc=<period_ms>`,
`dev::scenarios::moreosc_arm`) is the same check for More: RIGHT on the Quality row pushes the rung
page and LEFT pops it, every 900 ms. Same sequence with `--arm more=1 --arm moreosc=900`, reading
`route=player overlay=more`; `moreosc:` lines log each key. The Quality row is absent only under
Force Direct Play, so that setting must be off; the oscillator then logs `moreosc: no Quality row`
and pushes nothing, and the scene's `require_log` (`moreosc: depth=1`) fails the run rather than
grading a screen that never moved. Same 25 ms budget, same
"not yet measured on the TV".

Panel OFF does not stop presents on the player route: while the hardware video plane is bound
`ui::idle`'s `VIDEO_PLANE` gate forces a present every frame, so the frame times are the real
ones. (The general advice against `screen off` for fps scenes is about screens that DO rely on the
gate.) Sequence, each device command under the TV lock, the lock released between the device work
and any building or reading:

1. `tools/tv-lock.sh acquire --why 'submenu osc frame-time'`; wake the set.
2. On the host, mock PMS reachable from the TV: `python3 tests/mock_pms.py --host 0.0.0.0 --port
   <port> --media <mockverify dir>` (`tests/fixtures/make_fixtures.py --only mockverify`; an
   embedded subrip track and a sidecar), or `--extra-media <file.mkv>` with two subtitle languages
   to include the Other languages leg. Note the printed `rk=`.
3. `tools/tv-session.sh up --guest --mock --screen player=<rk> --arm menu=1 --arm submenuosc=900
   --arm framedrop=25` (the triggers are read once at boot, so they ride the `up`).
4. `tools/tv-session.sh screen off`, then `tools/tv-session.sh sound off`.
5. Let it run ~30 s after the menu is up, then `tools/tv-session.sh log` and read `loop=`,
   `worstframe=` and `FRAMEDROP` lines for `route=player overlay=menu` (the harness's grade is the
   2nd-highest post-warmup `worstframe=` against 25 ms). `submenuosc:` lines name any step the
   script skipped.
6. `tools/tv-session.sh down`, `sound on`, `screen on`, `tools/tv-lock.sh release`.

The same scene through `tests/run.py --filter track-menu-submenu-osc` needs the `manifest.local.json`
mapping for `movie_h264_ac3_1080p` and an account; the sequence above does not.
