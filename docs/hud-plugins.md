# Player-HUD plugins: format, renderer, registry, sideload, trust, PR breakdown

## Problem

Users want player HUDs that look different from the built-in one. The owner wants them to be able
to write a HUD design, share it, install one from a registry, and install one that is not in the
registry (sideload), on every install mode: a Homebrew Channel install (`/media/cryptofs/apps`,
`jail_native.conf`), an LG Developer Mode install (`/media/developer/apps`,
`jail_native_devmode.conf`), a rooted set, and both flavours (`com.beb.plxnative`,
`com.beb.plxnative.debug`).

Two acceptance tests bind the format: a user must be able to build the reference HUD (one image,
three stacked crops of ONE HUD: compact; chapters expanded; chapters plus info expanded) in format
1.0, and a model given the guide, the schema and one example must pass lint on the first or second
try. A third holds the renderer honest: the built-in HUD must be expressible in the same format and
draw pixel-identical through it.

The HUD is 3600 lines of Rust that mix state logic with presentation, and its geometry feeds the
pointer hit map, the FocusEngine groups, the caption lift and the two player menus. A plugin system
has to cut a versioned contract between "what the app knows and decides" and "how it is drawn",
without a second renderer, a script runtime, an ad-hoc focus or placement path, or IO in logic.

## Verified facts

Repository state: `git log --oneline -1` prints `414419c75`. Paths are under `rust-modules/` unless
they start with `docs/`, `ci/`, `locales/`, `site/`, `tests/` or `.github/`.

HUD drawing and state
- F1. `appkit/src/player_hud.rs` is 3600 lines (`wc -l`). The one draw entry point is `draw_hud(ps,
  row, up, slot, busy, focus, btn, tab, now, transport, measure, meta)` (`:2037-2050`). It returns
  early when `readout_owns_frame(busy)` (`:2067`), draws `draw_scrim`, then ONLY under
  `if transport` the title block, the row by `ControlSlot` (`UpNext` -> `up_next::draw`, `Skip` ->
  `skip_pill::draw`, `Discs` -> one `TransportButton` per `0..BTN_N`, `:2126-2142`) and the playbar
  with the scrub preview; the tab pills draw outside that branch (`:2160`).
- F2. `transport` is false while the Info card or the Chapters strip is open: `src/app/run.rs:2805-2816`
  computes `bare_middle = !matches!(player_overlay_kind, Some(Info) | Some(Chapters))` and pushes it;
  `screens/src/player/mod.rs:142-144` documents the field. So today an open Info or Chapters panel hides
  the title, the bar and the row, and a stand-in (Skip, Up Next) draws only when neither is open.
- F3. The draw path reads process globals, not values: `TX.paused` (`player_hud.rs:2095`, `:1847`); in
  `Playbar::live` `TX.scrub_ns` `:1826`, `playpos_ns()` `:1833`, `seek_display_ns()` `:1836`,
  `duration_ns()` `:1850`; `draw_scrub_preview` `TX.scrub_ns` `:2204`; `ClockWarm` `:93-97`; `slot()`
  `:905-906`; `busy()` `:1047`; `caption_geometry` `:151`; the bitmap path `:345`. The three player
  reads are `SHARED.*.load` (`media/src/player/mod.rs:682-696`).
- F4. App-owned state logic already sits in its own functions: `slot_for` `:878`, `slot` `:899`,
  `standin_left_the_ring` `:934`, `busy_surface` `:977`, `readout_owns_frame` `:1010`,
  `transport_hidden` `:1030`, `busy` `:1038`, `transport_mark` `:1112`, `FailureReadout` `:1227`,
  `scrub_speed`/`scrub_step_ns`/`scrub_clamp_target` `:1612-1668`.
- F5. Presentation pieces: `TransportRow` `:521`, `Controls` `:735`, `ControlSlot` `:791` (`item_rect`
  `:849`), `ctrl_slot*` `:695-717`, `disc_row`/`disc_items` `:679`/`:724`, `tab_*` `:1422-1461`,
  `draw_readout` `:1172`, `draw_title` `:1703`, `Playbar` `:1800`, `draw_clock` `:1561`.
- F6. Control vocabulary: `BTN_N = 5`, `BTN_PREVIOUS 0`, `BTN_NEXT 1`, `BTN_SUBTITLES 2`, `BTN_AUDIO 3`
  (`:493-497`), `BTN_MORE = BTN_N - 1` (`:670`), `pub enum Action { Previous, Next, Subtitles, Audio,
  More }` (`:731`). `Controls::enabled` (`:758-761`): Previous and Next by their own flags, the other
  three always; `Controls::pair` (`:755-757`) makes Previous/Next come and go together;
  `Controls::icon` swaps in `Icon::Restart` for Previous (`:772-776`).
- F7. Row identity is an INDEX: `Controls::action(i)` (`:767-771`), `ControlSlot::items()` 5/2/1
  (`:804-809`), `RowPop = CtlPop<{ BTN_N as usize }>` (`:512`), element ids `ELEM_SCRUB 0` `:1384`,
  `ELEM_ROW_BASE 10` `:1387`, `ELEM_TAB_BASE 20` `:1389`, `ELEM_FAILURE_BASE 30` `:1397`. Up Next
  items are `BTN_CREDITS 0`, `BTN_NEXT 1`, `PRIMARY_BTN` (`appkit/src/up_next.rs:50-57`).
- F8. Geometry constants: `CTRL_Y = BTN_Y = HUD_CONTROLS_TOP_Y` (792) and `CTRL_H = BTN_S`
  (`player_hud.rs:477-485`); `CTRL_RIGHT = SCR_W - MARGIN_X` (`:491`), "the discs' own edge";
  `SCRIM_H 470` (`:941`); `scrub_hit_rect` = `Rect::new(SB_X, SCR_H - 270, sb_w(), 160)`
  (`:1405-1407`), which covers the row and reaches the tabs (y 949, `:1439-1441`): overlap by design.
- F9. `HUD_TITLE_SZ: i32 = 54` is a documented carve-out from the type scale (`player_hud.rs:34-38`).
  `ui/src/CLAUDE.md` rule 2: "Exactly two carve-outs live outside the scale (the player-HUD display
  title `HUD_TITLE_SZ` and the subtitle caption) ... don't add a third".

Focus, keys and presses
- F10. The player's `Focusable` (`screens/src/player/mod.rs:1316-1410`) declares `GROUP_SCRUB` (Free,
  extent `scrub_hit_rect()`), `GROUP_ROW` (Row, `len = slot.items()`), `GROUP_TABS` (len 1 or 2),
  `GROUP_FAILURE` and `GROUP_REPAIR`; push order is the pointer's z-order, the scrubber first (#162,
  `:1313-1315`); `neighbour` walks the row with `next_enabled_index` (`:1389-1405`).
- F11. HUD travel between regions is still by hand: `key_updown` walks a ring (focus 0 scrub, 1 row,
  2 tabs; Up from the row hides the HUD) (`mod.rs:1043-1073`); `key_ok` arms the row, opens a tab's
  overlay, or toggles play (`:1010-1032`).
- F12. The `Key` enum (`ui/src/consts.rs:266-300`) is `Up, Down, Left { alt }, Right { alt }, Ok, Back,
  Play, Pause, PlayPause, Stop, Exit, PointerHidden, Other`; `alt` is true for the FF/REW transport codes
  (`:270-279`). No key exists for Previous/Next episode, Subtitles, Audio, Chapters, Info or More.
- F13. Key routes the HUD does not need a control for: the `Key::Play | Key::Pause | Key::PlayPause` arm
  (`mod.rs:913-925`, Play and PlayPause take a pending scrub target first, `:914`); OK on the scrubber
  or a hidden transport toggles play (`key_ok`'s last arm, test `ok_elsewhere_toggles_play_pause`
  `:2435-2442`); Left/Right scrub (`:977-991`); FF/REW taps and holds (`jump_key`,
  `screens/src/player/seek_jump.rs:48-80`); `ui/src/CLAUDE.md` idiom 3: "Dedicated FF/REW taps jump
  immediately; held keys use the shared scrub gesture."
- F14. The relative jump is `commit_jump` (`seek_jump.rs:37-46`): it returns when the duration is not
  known (`:39`), takes `seek_burst.jump(now, intended_pos_ns, step, duration)` (`:40`), hides the HUD
  (`:43`) and asks `CommitSeek`. The step is one setting for both directions (`plex/src/plex/session/prefs.rs:119-124`),
  `SkipInterval::LADDER` 5/10/15/30/60 s (`plex/src/plex/session.rs:1150-1172`), read by `scrub_step_ns()`
  (`player_hud.rs:1612-1614`).
- F15. The app performs a row press in `activate_player_row` (`src/app/playback.rs:1176-1208`):
  `Previous`/`Next` -> `queue_transport::step(QueueStep::Previous|Next)`, `Subtitles` ->
  `OverlayKind::Tracks { tab: 1 }`, `Audio` -> `Tracks { tab: 0 }`, `More` -> `More { quality: false }`.
  The track menu holds both tabs: "LEFT/RIGHT switch between the Audio and Subtitles panels on a root"
  (`appkit/src/track_menu.rs:4-5`).
- F16. The More menu only reports: "It only REPORTS the chosen [`Action`]; `app.rs` performs it"
  (`appkit/src/more_menu.rs:1-4`); Stats for nerds "is how this app gets bug reports off televisions
  nobody here owns" (`:7-13`). `Action { None, ToggleStats, SetQuality, SetSpeed, SendDiagnostics }`
  (`:90-103`), pages `MorePage { Quality, Speed }` (`:107-115`), performed by `apply_more_action`
  (`src/app/playback.rs:107-135`). Quality persists a profile preference (`persist_quality_choice` calls
  `choose::<QualityPref>`, `media/src/route/decision.rs:7377-7385`); Stats for nerds is "Deliberately
  NOT persisted across launches" (`src/app/diagnostics.rs:204-210`); a speed must be in the per-playback
  `speed_offer(ps)` and its pick reloads the stream (`media/src/player/mod.rs:1856-1872`); Send
  diagnostics is a Lab upload, `crate::lab::request_upload` (`playback.rs:133`).
- F17. The Info card's buttons report `InfoAction { FromBeginning, GoToDetail(rk), None }`, performed by
  `apply_info_action` (`playback.rs:1114-1158`): `request_seek(0)` plus resume; `exit_player` then the
  item's Detail page (it leaves PLAYBACK, not the app).
- F18. Overlays (`screens/src/player/overlay.rs:80-111`, census `OverlayKind::ALL`): `Tracks`, `Info`,
  `Chapters`, `More`, `Timing`. `hud_policy`: Timing `Hidden`, the rest `Lifted` (`:154-161`). Scrims
  (`:1052-1062`): Tracks `DIM_PLAYER` 0.58, More `DIM_SHEET` 0.50 (`ui/src/theme.rs:729`, `:734`), Info,
  Chapters and Timing none. Transport keys pass through every panel but More (`:657-672`). BACK
  dismisses (`:645-648`); Down at the edge of Chapters or Info dismisses it and seats the tabs
  (`PlayerReq::FocusTabs`, `:629-633`).
- F19. Up Next: `slot_for` depends only on the marker, the queue and the Next-episode mode
  (`player_hud.rs:878-892`). `countdown_may_run(bare_transport, row_focused, btn)` (`up_next.rs:105-125`)
  stops the clock behind any overlay: "A panel one presses OPEN is exactly as much 'engaging the
  transport' as an arrow key."

Menus and popovers
- F20. Both player menus are placed from classic constants, whichever disc opened them: the track menu
  `px = CTRL_RIGHT - pw`, bottom `PLAYER_MENU_BOTTOM`, height clamped to `bottom - 60`
  (`appkit/src/track_menu.rs:337-355`); the More menu the same x and bottom, height
  `clamp(120, bottom * 0.86)` (`appkit/src/more_menu.rs:576-606`, "Shares the track menu's right margin
  ... so opening one after the other does not make the panel hop"); their overscan rects use the same
  constants (`track_menu.rs:2626-2630`, `more_menu.rs:850-856`). `PLAYER_MENU_BOTTOM = SCR_H - 316`, 764,
  "~28px above the transport buttons" (`ui/src/theme.rs:197-198`). Both arrive by the fade+rise: the track
  menu by `Popover::RISE` (`track_menu.rs:2204`; `RISE = 20`, `ui/src/popover.rs:651`), the More menu by a
  literal `16.0` (`more_menu.rs:640`).
- F21. The shared anchored-panel placers are in `ui/src/popover.rs`: `under_or_above(a, w, h)` (`:283-301`:
  below the control when there is room, else `(a.y - PANEL_GAP - h).max(PANEL_EDGE)`, which can put a
  tall panel over its opener; x from the control's left edge clamped to `PANEL_EDGE_X`) and `hang_x(pill,
  w, keep)` (`:303-312`: the near edge is the opener's outer edge by screen half, clamped into `keep`).
  Callers: `screens/src/alt_sources.rs:410`, `screens/src/versions_panel.rs:95` (`under_or_above`),
  `screens/src/watchlist/options.rs:317`, `screens/src/library/menu.rs:470` (`hang_x`); pinned by
  `screens/src/alt_sources_tests.rs:674-736` and `ui/src/popover_host_tests.rs:254-258`. `PANEL_GAP =
  space::MD` (`:276`), `PANEL_EDGE = space::XL`, `PANEL_EDGE_X = MARGIN_X` (`:280-281`). Open styles
  `Appear` and `Morph` (`:266-273`), `open_from`/`track_opener` (`:448-458`), `Opener { rect, redraw }`
  (`:254-263`). The spacing ladder is XS 8, SM 16, MD 24, LG 40, XL 64 (`ui/src/theme.rs:178-186`).
- F22. Other HUD surfaces: the Timing capsule hides the HUD and sits at `CAPSULE_BOTTOM_Y` 836
  (`media/src/player/text_style.rs:70-72`, test `:295`); the scrub-preview card is centred over the knob,
  clamped to the bar and drawn above it, from arguments (`appkit/src/scrub_preview.rs:175-180`); the
  failure read-out owns the frame (F1); the repair alert is its own group (`mod.rs:1318-1322`). No hold
  hint is wired in the player screen (`grep -n 'HoldHint\|hold_hint' screens/src/player/mod.rs` prints
  nothing).

Sections today: the Info card and the Chapters strip
- F23. Info card (`appkit/src/info_panel.rs`): `card_geometry` x 80, width `SCR_W - 160`, top
  `SCR_H - 176 - 236` = 668, height 236, pad 28 (`:581-588`); `PANEL_TOP` ground, radius 28 (`:191-193`);
  a 320x180 still at `cx + pad` = 108 with a cover crop (`:197-221`); two buttons, From Beginning and Go
  to Show/Movie (`:566-575`, `:227-244`); title `TITLE` bold, one line (`:384-386`); synopsis `LABEL`, two
  lines (`:387-389`); a facts run of up to two genres, year and duration drawn BOLD in `TEXT_PRIMARY`
  (`Chip::run(&meta, false, 1, white, ..)`, `:300-315`; the bold argument `:747-753`), then the playback
  fact in regular `TEXT_TERTIARY` (`:336-345`), then badges: content rating, audio tag, CC, SDH, AD
  (`:347-361`), all fitted by the private `ChipRow` with a tail-first drop (issue #26, `:807`).
- F24. Chapters strip (`appkit/src/chapters_panel.rs`): 288x162 cards at `CH_TOP 684`, gap 24, radius 10
  (`:27-31`); its own scroll `Spring` and pop (`:46-50`) with a second-slot pin (`scroll_target`,
  `:228-236`); cards through `widgets::draw_card`, culled with `on_axis`, the focused one ringed in
  `CONTROL_RIM_FOCUS_UNKEYED` at 2.5 px (`:39-40`, `:126-170`); the name ABOVE the time, `LABEL` bold
  (primary when focused, else secondary) then `CAPTION` secondary (`:171-190`), the name elided by
  `elide_by` (`:179-183`); it seats on the chapter under the global `playpos_ns()` (`:54-61`).
- F25. Shared card strip: `cards::Shelf` owns the focus pop, scrolling to keep focus visible, culling,
  hit-map stops and `Focusable` placement (`ui/src/cards/mod.rs:1-9`); `Shelf::new(entry, style:
  &'static RowStyle)` (`ui/src/cards/shelf.rs:121`); `RowStyle` is `Clone, Copy` (`ui/src/card_row.rs:26-52`);
  the shared reveal scrolls minimally, "no slot pinning" (`card_row.rs:270-275`, `:543-560`);
  `CardSource::overlay` draws "on EVERY card after its body" (`cards/mod.rs:243-245`). `ui/src/CLAUDE.md`
  rule 4 names `cards::Shelf` for strips of tiles and says a focused card's text marquees, "never left
  ending in an ellipsis".

Text, formatting, languages
- F26. `ui/src/CLAUDE.md`, Localization: "App-owned text (`i18n::msg`, `plex::languages` names,
  `Quality::label`) never elides in any shipped language ... server- and user-owned text ... may", and
  "Adaptive text size or kerning is not used to make text fit"; the player HUD, `up_next` and chapters are
  listed under "Known unchecked surfaces". `i18n::SHIPPED` is generated from `locales/languages.json`, eleven
  tags: en es be fr it pt de hu ru pl nl (`python3 -I -c` over the file).
- F27. Formatting: `fmt::clock` prints `m:ss` (`h:mm:ss` past an hour) with no leading zero
  (`ui/src/fmt.rs:159-167`), so a chapter at 0 prints "0:00"; `fmt::duration` uses `core.duration_minutes`
  = "{minutes}m" in English (`locales/en/core.json:19-25`), so 56 minutes prints "56m"; the HUD's
  remaining clock carries a leading minus (`fmt_time`, `player_hud.rs:40-48`).

Captions
- F28. `media/src/player/text_style.rs` pins `size_scale` 0.78/1.0/1.25/1.5/1.8, `position_lift`
  0/160/320 and `(SUB_BASE_Y, SUB_CEIL_Y, HUD_LIFT) == (980, 780, 200)` (test `:288-295`);
  `CAPTION_LINE_PITCH 48` (`:21`), `CAPTION_ANCHOR_LINES 3` (`:30`). `HUD_CONTROLS_TOP_Y = SCR_H - 288` (`:56`),
  `SUB_CEIL_Y = SCR_H - 300` (`:64`); with the HUD up the baseline is `SUB_CEIL_Y - position_lift`
  (`player_hud.rs:155-164`). Readers of the band: `player_hud.rs:15`, `:155-163`, `:304-322`, `:477`;
  `text_style.rs:68`, `:192-195`; `screens/src/player/ass_subtitles.rs:271` (test `:398`);
  `media/src/player/ass/tests/synth.rs:443-447`; `plex/src/plex/session.rs:1076` (doc). Media cannot name
  appkit (`ci/module-layers.ini` `[media] uses = base machine platform gfx net plex telemetry data`).
  Captions draw first and the HUD over them (`screens/src/player/mod.rs:1513-1525`, `:1539-1541`), so
  today an open Info card (top 668) or strip (684) covers a caption lifted to 780.
- F29. `SAFE` = x 96..1824, y 54..1026 (`ui/src/consts.rs:32`, `:41`, `:94-99`); its doc exempts grounds:
  "a full-bleed hero backdrop ... a scrim, the page ground ... are supposed to reach the panel edge ...
  What must be inside is what the viewer has to read or press" (`:88-93`).

Layers, Sans-IO, settings, replay
- F30. `ci/module-layers.ini`: `[ui] uses = base machine platform gfx`; `[data]` members by name;
  `[appkit] members = appkit`; a new top-level module "fails the gate until it is placed here"; `[data]
  uses` has no `ui` (`:120`). `ui/src/CLAUDE.md:279`: the ui library is "never an application type".
- F31. `docs/sans-io.md:12-17`: a machine "starts no worker, makes no network, storage or LS2 call, and
  reads no wall clock or process-global registry"; ledgers under `ci/allow/{workers,globals,forks,fxlanes}/`;
  the store runner type is `super::adapters::stores::StoreRunner` (`src/app/bridge.rs:393`).
- F32. Settings scope: the ten player scalars are `Profile`, `hold_hint_taught` and `ui_resolution`
  `Install` (`plex/src/plex/session/prefs.rs:362-366`); profile prefs are account-bound and "sign-out drops
  the whole key" (`:355-357`); `PickerKind` is a static vocabulary (`screens/src/family.rs:61-77`); a page
  that builds a `TableView` ships a `fit_report` test (`screens/src/CLAUDE.md`).
- F33. Replay: three fixtures (`git ls-files tests/fixtures/replay`: `1-boot-home-chip-grid`,
  `6-settings-family`, `12-filmography-detail-return`); `grep -ci player` on each `rec-0000.jsonl` prints 0.
  `SCREEN_SHAPES` order is part of the hash (`screens/src/registry.rs:2389-2390`), pinned by
  `SCREEN_SHAPES_PIN` (`:2616`); a moved state shape refuses every anchor (`tests/fixtures/replay/README.md:115-127`).
- F34. `dev/scenarios.rs:1120` pins the HUD up for a headless capture; `.github/workflows/simulators.yml:99-108`
  runs the Linux `plxnative-sim` under `xvfb-run` and `tools/sim-smoke.py` captures a 1080p PNG.

Other subsystems the plan touches
- F35. Lower layers mirror HUD geometry (`text_style.rs:53-64`), and classic constants are read outside the
  HUD: `grep -rn 'player_hud::'` outside the module hits 28 files, among them `screens/src/detail/trailer.rs`
  (draws through `draw_scrim`, `draw_title`, `draw_playbar`, `:458-470`), `appkit/src/up_next.rs`
  (`CTRL_RIGHT`/`CTRL_Y`/`CTRL_H`, `:224-245`), `appkit/src/skip_pill.rs` (`ctrl_slot`, `:78-79`) and
  `src/app/overscan_audit_tests.rs`.
- F36. Icons are an `Icon` enum compiled in by `include_str!` (`ui/src/icons.rs:3`, `:299-312`), rasterized
  by `plx_gfx::svg::rasterize` (nanosvg, `gfx/src/svg.rs:13`, `src/svg.c`), cached in a `thread_local!`
  (`icons.rs:364-365`); the authoring contract is a white mask, one `<path>` per mark, subpaths clockwise
  (`:7-25`). No gear, list, equalizer or numbered seek glyph is built in (`:296-330`).
- F37. Widgets: `TransportButton` (`ui/src/widgets.rs:4881-4970`) has no bare-glyph rest face;
  `ControlStyle::{Accent, Custom { fill, ink }, Keyline, ..}` (`:7913-7940`); `widgets::hairline` is
  horizontal only (`:1895-1903`); `DottedRun` (`:1925`), `TextView::max_lines` (`ui/src/text_view.rs:203`),
  `draw_card` (`widgets.rs:1400`), `HStack` `Align::{Leading, Trailing, Centre}` (`ui/src/hstack.rs:119-127`),
  `VStack` `Anchor::{Top, Bottom, Centre}` (`ui/src/vstack.rs:28-35`). The playbar fills with
  `TEXT_PRIMARY`, its knob is Focused, Held or a Tick, and elapsed follows the head
  (`player_hud.rs:1785-1792`, `:1863`, `:1926-1958`).
- F38. Colour: `ACCENT = COOL_0` (`ui/src/theme.rs:266`), `RESUME_FILL = with_a(AMBER_300, 0.95)` "ours and
  tunable" (`:826-829`), `PASS_GOLD` is Plex's gold that "must not drift" (`:828-841`), the amber, red and
  green stops are "state and verdict, never decoration" (`:97-100`), `HAIRLINE = with_a(WHITE, 0.10)` (`:949`).
- F39. Storage: durable per-install files use probed candidate lists keyed by `app_id()`:
  `session_candidates()` (`base/src/paths.rs:550-600`) and `image_cache_candidates()` (`:630-650`,
  `/media/developer/temp/{id}-imgcache` owner-only); `app_id()` is the binary's directory
  (`docs/two-installs.md:83-85`); the app dir is "not writable by the jailed uid" (`paths.rs:531-532`);
  steerable builds put the runtime dir first (`:556-566`). Jail table: `docs/distribution.md` section 3.5
  (`:463-467`); the jail conf files are not vendored (`git ls-files | grep -i jail`); USB appears in neither
  `docs/distribution.md` nor `docs/two-installs.md` (`grep -i usb`).
- F40. Net: `net/src/net.rs:2102-2104` refuses a redirect-following request that carries caller headers;
  the only redirect door is `https_get_public(url)` with no headers and no body bound (`:2839-2840`),
  `PUBLIC_MAX_REDIRECTS = 5` (`:146`); curl 7.53.1 (`:2098`). App text entry is `src/textinput.rs`; a QR
  encoder is `ui/src/qr.rs`. The site is static GitHub Pages (`site/CNAME`, `.github/workflows/pages.yml`).
- F41. Crypto and parsing: no signature crate (`grep ed25519|minisign|ring =|signature` over every
  `Cargo.toml` prints nothing); `plx_base` has `sha256`, `b64`, `spki`; a duplicate-key-preserving JSON
  reader exists, private (`ui/src/rec/strict_json.rs`, `mod strict_json;` at `ui/src/rec.rs:40`); no JSON
  Schema crate (`grep -c 'name = "schemars"\|name = "jsonschema"' Cargo.lock` prints 0); `plx_ui` already
  depends on `serde_json` (`ui/Cargo.toml:50`). The lint host is the existing `plxnative-sim` bin
  (`Cargo.toml:64-67`, `required-features = ["hostsim"]`); dev triggers are the default `devtriggers`
  feature, absent from the shipping `--no-default-features` set (`Cargo.toml:73`, `:88`).

## Decisions (with reasons)

- D1. **A HUD package is declarative data, never code.** The app's own renderer compiles it once into a
  flat draw list over the shared widgets. No script runtime, no WASM, no second renderer. Reasons:
  webosbrew rule 3, "Be considerate to users' TV" (`docs/distribution.md:154`), on a 32-bit set with a
  two-buffer Mali pipeline; AGENTS.md, "never an ad-hoc mechanism" (a runtime would need its own focus,
  motion and text paths, which rule 4 forbids forking); Sans-IO (F31: data cannot do IO); data can be
  validated, previewed in CI and revoked by hash.
- D2. **One renderer.** The built-in HUD becomes the package `builtin:classic`, JSON embedded in the
  binary. When its captures are pixel-identical to today's, `draw_hud`'s hard-coded ARRANGEMENT is
  deleted; the widgets it arranges stay as the shared primitives the renderer composes (`draw_scrim`,
  `draw_title`, `Playbar`/`draw_playbar`, `TransportButton`, `TabPill`, `skip_pill::draw_at`). The trailer
  keeps drawing through those primitives at classic geometry (F35); a user HUD does not restyle trailers.
- D3. **The contract has two halves, versioned together**: `HudModel`, a per-frame read-only snapshot of
  exactly the values presentation reads (every F3 global becomes a field `src/app/run.rs` fills once a frame and
  pushes into the player screen as a value, as it already pushes `busy` and `transport`, `run.rs:2805-2816`: the screen
  is a machine and reads no global, `docs/sans-io.md`), and `HudGeometry`, what the compiled layout reports back (group, item and section rects, the
  caption band, the menu anchors, the safe-area rects). The FocusEngine groups, the pointer stops, the
  caption lift, both menus and the overscan audit read `HudGeometry` only. `HudMotion` (the springs,
  the `CtlPop`, the title reveal, the clock prewarm) is renderer-owned and NOT in the contract: a package
  chooses which widgets carry motion, never the motion.
- D4. **A control has an IDENTITY and a COMMAND, and the two are separate.** The identity is a
  package-chosen `id`, unique in the package, which FocusEngine and the pointer stops key on (compiled to
  an element id by declaration order, so a given package hash always maps the same). The command is one
  value of a closed, typed, app-owned vocabulary (section "Commands"), and several controls may carry
  the same command with different parameters: two seek buttons `{"command": "seek", "by": -10}` and
  `{"command": "seek", "by": 30}` give the reference its asymmetric 10/30 inside the package, with no
  settings split. A press is OK or a pointer click; it fires the command once. Hold-OK, the hold hint,
  each command's enabled predicate and what the command does are app-owned; a package cannot override
  them.
  - Rejected: free-form handlers, expressions or sequences of commands. A handler is code (D1); an
    expression language is a script runtime by another name; a sequence is a macro. And under Sans-IO a
    state change is an event on the owning machine (F31): a command is exactly one such event, named by
    the app, so a package can only ask for what the app already does.
- D5. **No optional category and no empty views; reachability is checked per COMMAND.** Every control
  is uniformly placed or not placed. An empty view would still be a FocusEngine stop and a pointer target
  with no pixels, and skipping it would need a special "empty item" case, which is the optional concept
  in disguise. Groups are built from the compiled geometry, so travel skips absent items by
  construction. The validator checks that every FUNCTION stays reachable, by its routes:
  - `toggle_play`: the Play, Pause and PlayPause keys and OK on the scrubber or the bare picture (F13);
  - `seek`: Left/Right, FF/REW and the held scrub (F13);
  - every other command has NO remote key (F12): Previous/Next episode, the track menu, the sections,
    More, the Info card's two actions.
  A key-less command that no control carries in any state the viewer can reach is FOLDED by the app into
  the app-owned More menu as a row (`more_menu::Action` gains a `Hud(HudCommand)` variant in place; the
  menu still only reports, and `apply_more_action` hands it to the same performer a placed control
  uses, F16). The ONE required control is the More opener (`open_more`), in the root state (compact,
  Discs slot): More holds Stats for nerds, which is how bug reports leave TVs (F16), and it is the home
  of every folded command, so it cannot fold into itself. Section "Reachability" defines the state graph;
  classic passes it as it is.
- D6. **Sections are designable presentation over app-owned logic, under one of two app-owned
  policies the package picks.** Chapters and Info become HUD sections (bands with a `when`), not modal
  overlays. Classic's panels are modal: Down at the panel's edge dismisses it and seats the tabs, and the
  transport middle is hidden while it is open (F2, F18). The reference keeps the row live beside the strip
  and opens both sections at once. So `layout.sections` is `"modal"` (one section at a time, opening one
  closes the other; focus that leaves the open section's own groups by a direction closes it; classic) or
  `"inline"` (any set may be open; focus travels freely; the reference). In both, OK on a control
  carrying `toggle_section` toggles that section, BACK closes the most recently opened section and then
  hides the HUD as today, the HUD stays up while a section is open, opening Chapters seats focus on the
  chapter containing the shown position (from `HudModel`, not the global, F24), opening Info seats on its
  first action when the package places one, and a failed playback closes every section (today's
  `survives_failure` is false for both, F18). Without a stated policy the FocusEngine would just move focus
  to the tabs and leave classic's strip open, a behaviour change.
- D7. **Stand-ins draw only with no section open, as today.** `slot_for` depends only on the marker
  (F19), so Skip or Up Next can become due while a section is open; today such a stand-in waits, because
  the row only draws under `transport` (F1, F2). That rule is kept: while any section is open the row
  keeps the package's controls (so every opener stays placed), the countdown cannot run (`bare_transport`
  is false behind any overlay or open section, F19), and the stand-in takes its segments when the last
  section closes, if the marker is still active.
- D8. **The caption band is derived from the COMPACT state only, one value per package.** Today the
  ceiling is the fixed 780 whenever the HUD is up, and an open card or strip covers a lifted caption (F28).
  A band that moved per section would move classic's captions in its info and chapters states and break
  pixel identity there. So the band is the union over the compact state's slot states (Discs, Up Next,
  Skip), measured on DRAWN extents, and sections may cover captions exactly as classic's do today. Lifting
  captions clear of an open section is a separate, owner-gated PR (PR17), because it changes classic's
  frames. Section "Captions" has the arithmetic.
- D9. **Menu placement is derived from the opener's compiled rect by ONE shared placer in
  `ui/src/popover.rs`, extended in place for every caller.** No player-local placer. A package cannot hint
  the side: in 1.0 the only anchor is `bottom` (D13), so a hint carries no information the room test does
  not already have, and a derived placement keeps every menu legal by construction. 1.1's top anchor can
  add a hint if the room test then needs one. Section "Menus and other HUD popovers".
- D10. **App-owned text never elides; server text may.** A text cell bound to an app label or an
  app-formatted value hugs its measured width (it takes no `w` and no `max_lines`), lint measures it in
  all eleven shipped languages, and overlap is the z-census's job (F26). Server text (`title.main`, chapter
  names, the synopsis) may set `w` and `max_lines`. The kicker mixes the two ("S2, E3 · " is the app's,
  the episode title the server's): only the server tail elides, and lint checks that the app prefix fits,
  as `Section::accessory_app_prefix` does for tables. The facts run drops whole chips tail first (classic's
  `ChipRow`, F23), so it never elides either.
- D11. **The chapter strip is a `cards::Shelf`.** Rule 4 names the Shelf for strips of tiles (F25), and
  what the strip needs (pop, scroll to keep focus visible, cull, stops, placement) is exactly the Shelf's
  contract. Its private scroll and pop (F24) go. Consequences, all visible in classic's chapters state and
  shown in that PR: the shared minimal reveal replaces the second-slot pin; a focused long chapter name
  marquees instead of eliding (F25); unfocused cards keep their elided persistent name and time through
  `CardSource::overlay`. Three extensions are made in place: `Shelf` takes its `RowStyle` by value instead of
  `&'static` (package-sized cards; `RowStyle` is `Copy`, F25), and a `RowStyle` focus rim for tiles over
  the video plane (`CONTROL_RIM_FOCUS_UNKEYED`, 2.5 px), and a `RowStyle` label alignment (`centre`, the default every
  existing row keeps, or `leading`, which skips `card_row::anchor_label`'s centred first case, `card_row.rs:1490-1507`),
  so classic's name and time stay at the card's leading edge as `chapters_panel.rs` draws them. UNVERIFIED: whether `card_row`'s tile paint can
  already draw that rim in place of the shader-baked ring.
- D12. **Colour: a package palette plus a token allow-list.** `"palette": {"gold": "#f2b535"}`, lower-case
  names (theme tokens are upper case), `#rrggbb` only, alpha on the referencing element, every entry used.
  Allowed tokens: `TEXT_PRIMARY TEXT_SECONDARY TEXT_TERTIARY ACCENT ACCENT_INK RAIL_TRACK HAIRLINE
  RESUME_FILL PANEL_TOP INK_DISABLED CONTROL_RIM_FOCUS_UNKEYED` (the PR fixes the list). Excluded:
  `PASS_GOLD`/`PASS_GOLD_INK` (another company's brand, F38), `DIAG_*`, `RATING_*` and the semantic
  amber/red/green stops. Rule 1 binds code: a palette is user data compiled into `PackInk`, the renderer
  writes no literal, and `builtin:classic` declares no palette (test `builtin_classic_has_no_palette`;
  `ui/src/CLAUDE.md` rule 1 gains that sentence). The reference uses a palette gold rather than
  `RESUME_FILL`, because a hue the token set lacks must not borrow a role that has another job.
- D13. **Placement is bottom-anchored in 1.0.** `layout.anchor` accepts only `"bottom"`. A top HUD needs a
  second caption keep-out that pushes top-positioned cues DOWN, a media-layer change to every F28 reader;
  format 1.1 can add it. Top DECOR with bottom controls is legal.
- D14. **Storage is per install and never shared between flavours**, for the session file's reason (F39):
  a debug build's unreviewed sideload must never reach stable.
- D15. **The HUD choice is a `Profile` setting** with an install-wide fallback key (F32). A missing or
  invalid selection falls back to `builtin:classic` for that playback; after a sign-out the profile key is
  gone (F32) and the fallback, default `builtin:classic`, applies. The selection resolves at playback start
  only, so an install never races a playing HUD.
- D16. **Registry: a Git repository for submission, a static content-addressed index for TVs.** TVs never
  read Git or the GitHub API.
- D17. **Sideload: typed address first, drop folder second**, both with primitives that work under both
  jails (F39, F40).

## Architecture

```
app (src/)               runners: HudStore effects -> net fetch / storage worker / dir scan (app/adapters/huds.rs);
                         one command performer shared by placed controls and folded More rows
screens/player           Focusable and pointer stops read HudGeometry; the page draws through player_hud::render::draw(&Compiled, &Frame, ..)
appkit/player_hud/       state.rs (F4 logic, unchanged), model.rs (HudModel), motion.rs (HudMotion),
                         render.rs (compiled draw list -> shared widgets), geometry.rs (HudGeometry),
                         command.rs (HudCommand, enabled predicates); builtin:classic is hud_format/fixtures/classic.plxhud.json,
                         embedded as hud_format::BUILTIN_CLASSIC
appkit/hud_format        schema descriptor table, strict parser, validator, SVG-subset checker, layout compiler
data/hudpacks            HudStore machine (new [data] member): installed set, selection (the index cache joins in PR13); pure; never parses
media/player/text_style  HudBand value type replaces the two constants as the caption ceiling input
ui/popover               the shared anchored placer (D9), used by the player menus and the four existing callers
```

- Layers (F30): `hud_format` is in appkit because the vocabulary (`ControlSlot`, the Up Next pair, the
  commands) is defined there and a ui copy would be a second definition. `data::hudpacks` is added to
  `[data] members` with a `docs/module-layers.md` line. It never parses: it hashes the bytes itself and asks
  for the verdict with a `HudEffect::Validate`; the runner calls `plx_appkit::hud_format::{parse, compile}`
  and answers `HudEvent::Validated { tag, sha, verdict }`, with the data-owned `Verdict { Loadable(Meta),
  Unloadable(Meta, Unloadable), Invalid }` (`Meta` is id, name, version, format; `Unloadable` is
  `NewerFormat`, `OlderFormat` or `AppRange`). The supported format range stays with the runner, which owns the
  validator: the store is handed no range.
- Per frame the renderer walks a flat `Vec<DrawOp>` and reads `HudModel` fields: no parsing, allocation or
  hashing; all text goes through `measure`, which `player_hud/render.rs` takes from its `Frame` and passes to each
  text draw.
- Package icons are rasterized through the same `plx_gfx::svg::rasterize` (F36); their textures belong to
  the `CompiledHud` instance `App` hands the player screen and are released when the HUD changes. The
  icon draw API takes `IconRef { Builtin(Icon), Pack(PackIcon) }`; no second cache, no new global.

## Package format 1.0

One UTF-8 JSON file, `*.plxhud.json`, assets inline, no archive (so no zip or tar parser on untrusted
input). Parsed strictly: unknown keys and duplicate keys are errors, through the duplicate-key-preserving
reader of F41, made a shared `pub` module of `plx_ui`. Coordinates are on the logical 1920x1080 canvas
(`base/src/surface.rs:47-48`); the existing scaling to the drawable size applies unchanged.

Top level: `format`, `id` (reverse-DNS, `[a-z0-9.-]`; the built-in's own file is the one other id that parses, `builtin:classic`, which proves nothing about where a file came from: a store that takes files from outside refuses it), `version` (semver), `name`, `author`, `license`,
optional `app` (`min`/`max`), optional `$schema` (the schema URL, for editors; accepted, its value ignored), optional
`palette`, `assets`, `layout`.

`layout`: `anchor` (`"bottom"`), `inset` (a space token or px above `SAFE`'s bottom), `x_inset` (from
`SAFE`'s sides), `band` (`"controls"` or `"block"`, D8), `sections` (`"modal"` or `"inline"`, D6),
`scrim`, one of `{"cover": "block", "extra": px}` (its top follows the compiled block top per state, so no author
types four heights) or `{"cover": "fixed", "height": px}` (one height from the screen bottom in every state, drawn before
any band: classic's `draw_scrim`, `SCRIM_H` 470, `player_hud.rs:941`, `:1678-1686`); `extra` and `height` are integers
in `0..=SCRIM_H`, so no package scrim rises further over its block than classic's whole scrim does over the screen
bottom, and its blended area is a cost term (Cost); `block` (bands, top to bottom, in one bottom-anchored `VStack`),
`up_next`.

Band: `id`, optional `when`, `gap_before` (token or px), `gap` (between packed cells), optional `z`, `chrome`
(`fades`, the default, or `steady`: while a scrub is live a `fades` band draws on `chrome.alpha` and a `steady` one at
full alpha, today's split between `pc` and `p`, `player_hud.rs:2075-2081`, `:2146-2156`; the band holding the `playbar`
must be `steady`, a lint error otherwise, because the bar, the clocks and the preview are what a scrub shows), `cells`.
A cell has `align` (`start`, `center`, `end`; cells of one alignment pack LEFT TO RIGHT in array order whatever the
alignment, so an end-aligned `[rule, facts]` draws the rule left of the facts), `valign` (`top`, `center`, `bottom`), an
optional `when`, optional `gap_before`/`gap_after` (token or px, with `layout::Item::before`/`after`'s exact meaning,
`ui/src/hstack.rs:94-102`, `:434-444`: the gap to the previous or next PRESENT neighbour in place of the stack's own;
also legal on a `vstack` cell and on a control item), and exactly one of:
- `text` {`bind`, `size`, `ink`, `alpha`, `bold`, `leading` (px: the line pitch, and a one-line text's box height;
  default the rung's line height; the box top is the y the renderer hands `p.text`, the value classic passes today, so
  classic's kicker at `SCR_H - 312` and title at `SCR_H - 278` are boxes of 34 and 54, `player_hud.rs:1732`, `:1763`;
  UNVERIFIED whether that y is the glyph top or a baseline, which changes no offset), and for server text only `w` and
  `max_lines` (1 elides through `measure.fit_line`, more wraps through `TextView::max_lines`), D10};
- `image` {`bind`, `w`, `h`, `radius`, `crop`: `"cover"`} through the image cache;
- `icon` {`asset`, `size`, `ink`}; `rule` {`axis` `h`|`v`, `length`, `ink`} (`widgets::hairline` gains the
  axis in place, F37);
- `rect` {`ink`, `radius`, `pad`, `bleed`, `gap` (between its cells), optional `h` (px: a fixed panel height, its cells
  centred in it, classic's card of 236, `info_panel.rs:581-588`), `cells`}: a filled panel. Its content box is the ground
  inset by `pad` and must lie inside `SAFE` (so `bleed <= pad`; classic: 80 + 1760 - 28 = 1812 <= 1824). It is laid out inside
  `SAFE` and whose ground extends `bleed` px past it, never past the screen. SAFE's own doc exempts a ground
  (F29); its content (text, images, controls) stays inside;
- `facts` {`bind`: `info.facts`, `order` (ordered subset of `duration year genres content_rating`),
  `genres_max`, `bold`, `ink`, `playback_fact` (bool; drawn regular in `TEXT_TERTIARY` as today),
  `badges` (ordered subset of `content_rating audio_format cc sdh ad`), `size`}: content rating appears
  in `order` OR `badges`, never both; all of it is fitted by `ChipRow` (moved out of `info_panel.rs` into a
  shared appkit module), which drops whole chips tail first;
- `vstack` {`gap`, `w`, `cells`}; `spacer` {`w`};
- `playbar` {`w`, `fill`, `track`, `thickness`, `knob` (`tick` classic, `always`), `clocks` (`follow_head`
  classic, `ends`), `clock_size`, `mark` (`after_elapsed`, `busy_only`: the paused glyph may go when a
  `toggle_play` control is placed; the busy spinner never goes), `grab` {`above`, `below`} (px past the track's top and
  bottom edges, default 72 and 80: classic's grab band `(SB_X, SCR_H - 270, sb_w, 160)` around the track at
  `SB_Y = SCR_H - 198`, 8 tall, `player_hud.rs:474-475`, `:1405-1407`; registered as the lowest stop, #162)}: exactly one,
  the scrub group. Its cell is the track plus the line the clocks are drawn on: as tall as `CLOCK_TOP` (30 px, the
  clocks' line top below the track's top, the one constant `draw_playbar_look` draws at) plus `clock_size` x
  `CLOCK_LINE` (1.25, a ratio so a measurer cannot move a band), or `thickness` if that is taller, so no band under
  the bar lands on the clocks. The compile also emits the two clocks as rects on the line (the widest string the app
  can print, `H:MM:SS`, in the weight each is drawn in; under `follow_head` the elapsed clock travels the track, so
  its rect is the track's width), which I4 reads as decor and I10 judges as app text. The knob and mark overhang the
  track;
- `segment` {`id`, `gap`, `items`}: one `HStack` of controls (below);
- `strip` {`bind`: `chapters`, `w`, `card_w`, `card_h`, `gap`, `radius`, `focus`: `"ring"`, `label_align` (`centre`,
  the Shelf's default, or `leading`, D11), `card`: {`art`, `overlay`, `below`, `below_gap`}}: the chapters
  `cards::Shelf` (D11); scroll and seat app-owned. Its cards may run past `SAFE`'s side edges, culled at the screen edge
  as today (`chapters_panel.rs:140-147`; SAFE's own doc lets "a shelf peeking off" reach the edge, `ui/src/consts.rs:88-93`),
  and the focused card is always revealed inside `SAFE` (I5);
- `actions` {`axis` (`row`, the default, or `column`), `gap`, `item_h`, `min_w`, `items`}: controls inside a section,
  items shaped as a segment's. Every `button` in it takes one width: the widest label over EVERY variant its slot can
  show, shown or not, floored at `min_w`, so the column never changes width with the item (classic: floor 352, 70 tall,
  16 apart, measured over From the Beginning, Go to Show and Go to Movie, `info_panel.rs:594-608`).

Fill. A cell or `vstack` with `w: "fill"` takes the band's free width: from its own start edge to the nearest edge of
the next PRESENT cell in packing order (for a later control, its rect scaled by `max(its scale, CTRL_FOCUS_SCALE)`; for
a slot stand-in that replaces a segment of this band, the leftmost of every rect it draws, Up Next's still, caption and
buttons or Skip's pill), minus the band's `gap`, never below 0. It is `title_right` (`player_hud.rs:1769-1781`) made the
rule, so classic's title, in ONE band with the end-aligned row and `gap: MD`, reaches exactly where it does today; a
cell's room never depends on another band.

Control item: `id`, `command` plus that command's typed parameters as sibling keys, optional `when` (an absent item
collapses as `Item::absent` does, so classic's Chapters pill exists only with chapters, `tab_labels`,
`player_hud.rs:1430-1435`), optional `gap_before`/`gap_after` (classic's Next carries `gap_after: 40`,
`2 x CONTROL_GAP`, `player_hud.rs:724-727`), `widget` (`disc`
through `TransportButton`, `pill` through `TabPill` labelled by the command's app string, `button`
through `widgets::Button`), `size` (px), `rest` (`film` today's, `bare` glyph only, `solid` the focused
colours at rest, `custom` with allowed inks), `focus` (`accent` today's, `ring`: rest face plus the white
focus ring), `face` (optional: the command's classic glyph when absent). A pair that draws one face twice (`solid` with `accent`) is rejected. Faces: an asset
or `builtin:<icon>`; `toggle_play` takes `{playing, paused}`; `previous` takes `{default, restart}`;
`toggle_stats` takes `{on, off}`; a `seek` with `by: "setting"` takes `{by_interval: {...}, default}`. A
glyph's visual size is its viewBox padding over the shared `DISC_ICON_RATIO` (`ui/src/widgets.rs:1394`).
`TransportButton` gains the `bare` rest face and the `ring` focus face in place.
A `toggle_section` item draws a SELECTED face while its section is open (`hud_tab_state`'s second flag,
`player_hud.rs:1587-1590`): `pill` uses `TabPill`'s own selected face, `disc` its `solid` face; the focus face wins when
both hold. A control's `size` is a plain number, never state-keyed, at or above `MIN_CONTROL_SIZE` (26 px, derived
in PR5d from the narrowest seat the app's own screens place and pinned by a test, see "Unverified items"; it admits the
reference's 52). A control has no alpha key and the palette is `#rrggbb` (D12), but an allow-list ink
can be translucent (`HAIRLINE` is `with_a(WHITE, 0.10)`, `ui/src/theme.rs:949`), so a rest face's GLYPH ink, composited
at its alpha, must reach 3:1 contrast against black (relative luminance at least 0.10), the darkest ground a scrim over a
dark frame gives it, or, on an opaque ground of its own (`solid`), 3:1 against that ground; lint error otherwise. Classic's
white glyphs and the reference's `solid` disc (ACCENT with `NEUTRAL_950` ink) pass; a `HAIRLINE` glyph fails. The 3:1
figure is WCAG 2.1's non-text contrast floor (UNVERIFIED here: no web access).

Segments of one band form ONE FocusEngine Row group; Left/Right crosses segments in x order. Segments in
other bands are other groups. Previous and Next come and go together (F6), so a centred hero stays centred
for a movie; an absent item collapses.

### Commands

Each maps to an action the app already performs; the performer is shared with the folded More rows (D5).

| Command | Parameters | The app performs (evidence) | Enabled (app-owned) | Remote route |
|---|---|---|---|---|
| `toggle_play` | - | the `Key::Play|Pause|PlayPause` arm, extracted to one function, PlayPause path incl. `take_target` (F13) | always | keys, OK on picture |
| `seek` | `by`: a nonzero integer of seconds, whose sign is the direction (then `dir` is forbidden), or `"setting"`, which requires `dir`: `back` or `forward` | `commit_jump`'s `SeekBurst` and `CommitSeek`, extracted with the signed step in ns as a parameter (today `signed_step(forward)` = `scrub_step_ns() x +-1`, `seek_jump.rs:16-18`; a literal `by` passes `by x 10^9`) and a keep-HUD flag (today `self.hud.hide(false)`, `:43`): a control press keeps the HUD up and extends its linger, the hidden-HUD tap still hides it (F14) | duration known (`seek_jump.rs:39-40`) | Left/Right, FF/REW, hold |
| `previous`, `next` | - | `queue_transport::step` (F15) | `Controls::for_item` (`player_hud.rs:749-752`): the pair is absent for a non-episode or with neither neighbour; Previous is enabled by `previous || restart` (a restart target counts), Next by `next` (F6) | none |
| `open_tracks` | `tab`: `subtitles`, `audio` | `OverlayKind::Tracks { tab: 1 | 0 }` (F15) | always (F6) | none |
| `open_more` | `page`: `root` (default), `quality`, `speed` | `OverlayKind::More { page }`; `quality` is what was `More { quality: true }` (F18); `speed` is the address PR3 added by turning that bool into `page: Option<MorePage>` (F16) | always | none |
| `toggle_section` | `section`: `chapters`, `info` | opens or closes the section (D6) | chapters: the playing item has chapters (`chapters_panel::has_chapters`) | none |
| `from_beginning` | - | `InfoAction::FromBeginning` (F17) | always | none |
| `go_to_detail` | - | `InfoAction::GoToDetail(rk)`: leaves playback, opens the item's page (F17) | the playing item has a ratingKey | none |
| `toggle_stats` | - | `more_menu::Action::ToggleStats` (F16) | always | none |

- `seek.by`: a literal has a static face, so no ladder-keyed map is needed, and a new rung on the app's
  ladder can never invalidate a published package. `"setting"` follows the viewer's Skip interval (F14);
  its face map needs `default`, and a missing rung is a lint WARNING, never a load failure. A literal is
  not capped: the target is clamped to the content at the press, as every jump is (`seek_burst.jump(..,
  duration)`, `seek_jump.rs:41`; `Scrub::clamp_target`, `:79`); zero is rejected because it does nothing.
- Up Next's two buttons keep FIXED commands, `watch_credits` and `play_next` (F7), because the countdown's
  meaning depends on which one is primary (F19). The Skip pill stays an app-owned stand-in.
- More rows as direct buttons: `toggle_stats` is in (session-only, F16). `set_quality` is out: it writes a
  profile preference (F16), and a setting change is excluded (below); `open_more {page: quality}` reaches it.
  `set_speed` is out of 1.0: the speeds a playback can take are per playback (`speed_offer(ps)`, F16), so a
  fixed-rate button would be inert on many items, and a pick reloads the stream; `open_more {page: speed}`
  reaches it. `send_diagnostics` is out: it starts a network upload of its own (F16).
- Excluded from the vocabulary, with the reason:
  - anything that writes the household's Plex record (mark watched or unwatched, rate, Watchlist, Remove
    from Deck): a third-party file must never be one press away from a write to a real account (AGENTS.md,
    "Never write to a real person's Plex account");
  - anything that changes a setting, exits the app, starts a network request of its own, or deletes
    (`go_to_detail` leaves playback for the playing item's own page, an app-chosen destination, as classic's Info
    button does; it does not exit the app, and BACK and Stop already leave playback from every state);
  - macros or multi-command presses, which are scripting in disguise (D4).
- A new command is a format MINOR (Versioning).

Conditions (`when`, and the keys of a state-keyed value): `panel.chapters`, `panel.info`, `sections` (any
open), `chapters.present`, `title.is_episode` (DEFINED as `is_real_episode`, the gate classic's kicker style uses,
`player_hud.rs:2097`: a show's trailer is `is_episode` but has no real address, so it takes the context style),
`transport.paused`, `slot.discs`, `slot.up_next`,
`slot.skip`, and in a strip card `item.is_current`, `item.focused`; a string or an array that must all
hold, each optionally prefixed `!`. That is the whole condition language. A number (not a control's `size`), an ink, a
gap or `bold` may be state-keyed one level deep: `{"value": v, "if": {"<condition>": v2, ...}}`; when several
conditions hold, the FIRST in document order wins (UNVERIFIED that the strict reader of F41 exposes pair order; if not,
the PR makes it return pairs in order).

Bindings: `title.kicker`, `title.main`, `title.is_episode`, `transport.paused`, `playbar.pos`, `playbar.dur`,
`playbar.live`, `playbar.seek_display`, `scrub.live`, `scrub.pos`, `playbar.busy`, `playbar.mark`,
`chrome.alpha`, `chapters.present`, `slot.kind`, `focus.id`, `stats.on`, `seek.interval`; the chapter list
`chapters` with `item.thumb`, `item.start` (`fmt::clock`), `item.title` (the server's chapter name, which may elide; when it is empty, the app's
`widgets_chapters_number(index)`, `chapters_panel.rs:176-179`, which is APP text: drawn whole, never elided, and lint
measures it with a 3-digit N at the cell's width in all eleven languages, an error if it does not fit), `item.is_current`
(from `HudModel`), `item.focused`; `info.still`, `info.title`, `info.summary`, `info.facts`; app labels
`label.chapters`, `label.info` (the existing tab strings); Up Next draws its own inside (below), so it has no bindings.

Sizes are `theme::size` rung names, plus `HUD_TITLE`, which names the existing 54 px carve-out (F9) and is
legal only on a text cell bound to `title.main`, that carve-out's own role: it exposes the carve-out, it
does not add a third. Weights are the two the app ships (`bold`). No fonts.

Up Next (`layout.up_next`): `replaces` (segment ids), `band`, `align`, `still` (bool), `caption` (bool),
`watch_credits` and `play_next` {`rest`, `focus`}, `countdown`
(`fill_sweep` classic `Button::progress`, `ring` extending it in place, `bar` through
`widgets::progress_bar` (`ui/src/widgets.rs:1478`), `numeric` through the new i18n key
`player.up_next.seconds_left` with a `fit_report`). While `slot.kind` is UpNext or Skip in the compact
state, the named segments leave the frame and the focus map; the others stay. The block's INSIDE is app-owned and
drawn by `up_next::draw_over` for every package: the caption is the one template `widgets_next_caption` (CAPTION, bold,
`TEXT_PRIMARY`, in a 460 box, elided; `up_next.rs:290-298`, `:320-330`), the still a 16:9 `draw_card` at the primary's
width, radius 10, the caption `CAP_GAP` 8 above the still's foot and `BTN_GAP` 24 above the buttons, the pills `PILL_GAP`
apart (`up_next.rs:194-200`, `:226-258`), the primary floored at the floor width (below), Watch Credits at its own
label's width (`:259-266`). The column overhangs its band upward from the replaced segment and adds nothing to the band's
height; I4 refuses an overhang over any drawn interactive rect, and under `band: "block"` it counts as the compact
block's decor (Captions). `align` places it: `end` at the replaced segments' end edge (classic, `CTRL_RIGHT`), `center` on their centre,
`start` on their start edge, and the caption's text alignment follows (right, centre, left): the compile applies
`align` (`standin` in `hud_format/compile.rs`) over `up_next::layout_at`. A package chooses whether the still and the caption draw, never their inside. The FLOOR exists where it
means what it does today, "never narrower than the pair it replaces, so the row does not visibly shrink"
(`up_next.rs:252-259`): when `replaces` names ONE start- or end-aligned segment, the floor is that segment's width with
every item that CAN be absent collapsed (a Previous/Next member, an item with `when`), whatever the current item holds;
for classic that is `disc_row()` over `disc_items(false)`, the three always-present discs, `ctrl_row_w` = 3 x 64 + 2 x 20 =
232 (`player_hud.rs:672-683`, `:717-719`). A centred or multi-segment replace has no floor: the primary takes its own
label's width (`classic::stand_in_w` with no floor). The Skip pill is `classic::stand_in_w` of ITS OWN label
(`standin` in `hud_format/compile.rs`), under the same floor rule, at the same aligned edge. Logic stays app-owned: when it appears, the countdown's
timing and cancel rules (`up_next.rs:105-125`, production sites `src/app/run.rs:2001`, `:2026-2045`,
`src/app/playback.rs:939`), what the buttons do.

Assets: SVG only, under the icon contract (F36): `<svg viewBox>` and `<path d>` only; no `href`, `<image>`,
`<text>`, `<style>`, entities or external references. A Rust tokenizer checks the subset before nanosvg sees
the bytes, for every source, because `src/svg.c` is a C parser and the input is attacker-controlled.

SVG assets are MASKS. The pinned nanosvg (commit 239e102, `ci/dependencies.json:132-141`, fetched into the gitignored
`vendor/nanosvg`, `Makefile:906-908`) does parse and fill gradients: the paint types `NSVG_PAINT_LINEAR_GRADIENT` and
`NSVG_PAINT_RADIAL_GRADIENT` (`nanosvg.h:78-79`), `linearGradient`/`radialGradient` parsing (`:2842-2845`), the gradient
fills (`nanosvgrast.h:1030`, `:1074`). In the fetched header (the main checkout's copy, whose sha256 begins
`e34fd5d084be106c`, the pinned hash) `grep -c` counts 0 for each of `clipPath`, `mask`, `filter`, `"use"`, `pattern`
and `"image"`, so none of those is supported. A gradient's COLOUR still does not survive, because the app keeps only
coverage: an icon is authored white and tinted per state (`ui/src/icons.rs:9-10`: "only alpha survives, so gradients
and multi-colour fills are wasted"; `src/svg.c:29-32`). An OPACITY ramp would survive, since nanosvg parses `opacity`,
`fill-opacity` and `stop-opacity` (`nanosvg.h:1882`, `:1884`, `:1919`) and coverage is the alpha the shader keeps; the
subset rejects it for the contrast rule, which needs one ink at one alpha per glyph. Decisions:
- 1.0 keeps the mask contract for EVERY asset (control faces, icons in cells and card overlays): a face changes ink
  between rest and focus (I8), and the rest-glyph contrast rule needs one ink per glyph. The subset above already
  admits no gradient element and no colour attribute; lint names the case (`HUD-E0701`, `HUD-E0702`, LLM authoring) so
  an author learns why.
- The library is not replaced in 1.0: it already draws gradients, the contract rather than the library drops colour,
  and a new SVG parser is a new attacker-facing dependency on the 32-bit ARM binary.
- A later minor may add a full-colour `art` asset kind: decor only (an `image`-like cell or a card overlay, never a
  control face, so I8 and the contrast rule are untouched), rasterized once per size through the same nanosvg with its
  colour kept (`svg_rasterize_rgba` already returns RGBA, `src/svg.c:29-33`), its subset widened by the gradient
  elements, `stop` and the fill attributes, and counted in the cost model (texture bytes, blended area).
- Replacing nanosvg (resvg, for example) is considered only if authors need filters, masks or `clipPath`: a new
  dependency on the ARM binary, so a `make build-bench` table and a release-profile size measurement (AGENTS.md, build
  section), and PR16's fuzzing over it.
- The scrim and the bar's gradients are app-drawn (`draw_scrim` is one two-colour `p.rect`, `player_hud.rs:1678-1686`;
  the bar's fill is a `Playbar` field), so a 1.0 package needs no SVG gradient for them.

### Reachability over the HUD's state graph

States: (section set, slot), under a valuation of the content conditions (R1). The section set is `{}` (compact) or what `layout.sections` allows (modal: one
section at a time; inline: any subset of the sections the package lays out); the slot is Discs, Up Next or
Skip. Edges are app-owned: OK or a click on a `toggle_section` control; BACK (closes the latest section,
then hides the HUD); for modal sections, a direction that leaves the section's groups (closes it); the slot
changes involuntarily with the marker (`slot_for`, F19), and every stand-in slot returns to Discs when its
marker ends or Watch Credits dismisses the offer.

- R1. `open_more` is placed, at or above `MIN_CONTROL_SIZE`, in the root state (compact, Discs) under EVERY valuation
  of the content conditions the package names (`chapters.present`, `title.is_episode`; a placed `previous` or `next`
  names `title.is_episode`, because the app drops that pair for a non-episode); `transport.paused` counts as
  holding either way, because `toggle_play`'s keys flip it (F13). The compiler enumerates the valuations (two per
  content condition, so at most four today). The only hard requirement: a More cell with `when: "title.is_episode"`
  fails, since every movie would lose More and with it Stats for nerds, the bug-report path (F16).
- R2. For every other key-less command (D5), the compiler computes whether some state reachable from the
  root by the edges above places an enabled-capable control carrying it, under EVERY valuation in which the command is
  enabled (a valuation where its predicate fails, Previous for a movie, asks nothing); a command placed under only some
  of them folds. Each command that none places is
  folded into More's root, after Quality and Speed and before Options (the More fold PR adds the rows, their
  i18n keys in all eleven catalogs and a `fit_report` over More's FormTable; the section rows reuse
  `widgets_player_chapters`/`widgets_player_info` (`player_hud.rs:1433-1434`) and the Info actions reuse
  their button strings (`info_panel.rs:566-575`), so only Previous, Next and the track-menu row need new keys). A command that already has a
  More row (`toggle_stats`, the Options switch) never folds. Reachability can therefore never fail except
  by R1; lint lists the folded commands as information.
- R3. A `toggle_section {section: s}` control implies a band with `when: panel.s` (lint error otherwise: an
  opener with nothing to open); a section band with no opener is reachable through the fold.
- R4. Both Up Next buttons are placed in `layout.up_next`, the countdown presentation sits on or in
  `play_next`'s rect and is never absent while running (an auto-advance the viewer cannot see coming is the
  failure).
- Classic passes as it is: it places previous, next, subtitles, audio and more in the root; its tab pills
  carry `toggle_section` in every state; its Info section places `from_beginning` and `go_to_detail`. Its
  modal sections hide the row, and BACK returns to compact (F18); its stand-ins replace the discs and the
  slot returns to Discs. Nothing folds.
- The reference passes: it places everything but `open_tracks {tab: audio}`, which it reaches anyway because
  the track menu holds both tabs (F15): the fold treats `open_tracks` as ONE command whose tab is an opening
  address, so it folds only when neither tab is placed. `from_beginning` and `go_to_detail` fold into More.

## Versioning and mismatch behaviour

1. `format` = MAJOR.MINOR of the schema, bindings and command vocabulary. A MINOR only adds (a binding, an
   optional key, a command, a condition). A MAJOR may remove or change. The app compiles in the range it
   supports, for example `{1: 0..=3}`.
2. `version` = the plugin's semver; it only orders updates of one `id`.
3. `app.min`/`app.max` (optional) = an app-release range, an escape hatch for "this release renders me
   wrong"; the registry index can carry it per entry.

Decided at install and at selection, never per frame: an unsupported major, or a minor above the app's, is
rejected ("Made for a newer PlxNative" / "Uses an older HUD format"). Strict parsing makes that mandatory: an
older app cannot know whether an unknown key is cosmetic. An app update that drops a major keeps the files
and marks them unloadable; a selected HUD that becomes unloadable falls back to `builtin:classic` with one
`hud: fallback` event-log line. Playback never fails because of a HUD. A package that does not fit the TV's current
language is unloadable in that language only (Text fit).

## Text fit: the app is the enforcement point

App text hugs and never elides (D10), so "fits" means: in every state, slot and language, the compiled frame passes I4,
I5 and I10 with that language's measured widths (a label that grows into a control, past `SAFE`, or past a box it lives
in, a `vstack` of fixed `w`, a strip card for `item.title`'s fallback `widgets_chapters_number(N)`, an `actions` button, fails). The registry can check only
what it sees, a file and the simulator it pins; it cannot see a sideload, a TV's language or a later app release. The
app sees all three at the moment it would draw, so the app is the enforcement point and the same compiler runs in
three places:
1. `plxnative-sim --hud-lint --lang all`, locally or in a model's lint loop (LLM authoring);
2. registry CI, through the simulator pinned to an app release, as a required check that refuses the merge;
3. the TV, which compiles at install, at selection and when playback resolves the selection (D15), and again after a
   language change or an app update, with its OWN `Measure` for its current language. Its result is cached per
   (package sha256, app version, language). The TV's compile is the authority; 1 and 2 are early warnings, so an
   unreviewed sideload is bound exactly as a registry package is.

A package that fails on the TV does not load in that language: playback uses `builtin:classic`, one `hud: fallback`
event-log line names the package, the language and the first finding's code, and Settings > Player HUD marks the
package's row with the reason (a new key `settings.hud.unfit_language`, "Doesn't fit in {language}", in all eleven
catalogs with a `fit_report`). The selection is kept, so a change back to a language it fits restores it.

The gap: an app release can lengthen a translation or add a language, so a package that passed can fail later.
- (a) Re-lint on every app release: YES. A job in `plxnative-huds`, triggered by a `repository_dispatch` from this
  repo's release workflow and also weekly as a backstop (a missed dispatch must not leave the index stale), re-lints
  the latest version of every entry with the new release's simulator in all eleven languages. Per entry it records the
  result in the index as `unfit: [{"from": "<app release>", "langs": [...]}]`; when every language fails, or a
  non-text invariant does, it writes `app.max` = the last release that passed, the field Versioning already defines.
  It opens an issue on the registry repo mentioning the author, with the lint JSON per language. The gallery hides an
  entry whose `unfit` covers the TV's app version and current language, or offers a newer `version` of that id that
  passes as its update; an installed copy keeps compiling locally and falls back per (b). Why a job: the failure is
  caused by the app's release, not by the author's push, and per-PR CI never runs again on an unchanged entry. In this
  repo, every PR's CI lints `site/hud/examples/` (PR6) and `builtin:classic` (`builtin_classic_passes_every_invariant`,
  PR5) in all eleven languages, so a string change that breaks a shipped package fails before any release.
- (b) The fallback is per language. A failure is a property of (package, app release, language); failing the package
  everywhere would punish ten languages for one translation, and keeping the selection lets a later fix (a new app
  release, a package update, a language switch) restore it with no step from the viewer.
- (c) Headroom. No margin for future translation edits: their size has no distribution to derive one from (catalog
  strings change one by one, by hand), so any figure would be arbitrary, and it would reject designs that fit every
  shipped language today; (a) and (b) turn a later change into a visible, attributed event instead of a guess. One
  margin IS derivable and is required, in registry CI only: the measurement skew between the pinned simulator and the
  TV. PR12 measures the lint's app-text corpus (every app string a package can show, at every rung lint allows it, in
  all eleven languages, `item.title`'s fallback `widgets_chapters_number(N)` up to 3 digits) through the simulator and through the TV's own `Measure` (a dev
  trigger under `devtriggers` dumps the widths under `--guest --mock`, the TV lock held), and sets `CI_TEXT_SKEW`, per
  rung, to the largest observed `tv_w - sim_w` rounded up to a whole px. Registry CI then requires app text to fit with
  that many px to spare (`HUD-W0602`, a warning locally, an error in registry CI). Until it is measured the margin is 0
  and the TV decides. The TV applies no margin: it measures what it draws.

Do the simulator and the TV measure alike? The font FILES are the same: the app bundles `appfont.ttf` and
`appfont-bold.ttf` (Inter), `appfont-cjk.ttf` and `appfont-emoji.ttf` (`rust-modules/gfx/src/text.rs:32-35`, `:94-104`),
opened through SDL_ttf's `TTF_OpenFont` with `TTF_HINTING_LIGHT` (`:848`, `:872`). The RASTERIZER builds are not: the
TV links the firmware's `libSDL2_ttf` (`-lSDL2_ttf` in `LIBS_REAL`, `Makefile:401`, against the sysroot), the simulator
the host's, found through pkg-config (`rust-modules/build_support/host_link.rs:45`, `:151-159`). `ci/dependencies.json`
pins FreeType 2.15.0 only in the `libass` group (`:38-44`; static inside libass, `Makefile:916-917`), and has no SDL_ttf
entry (`grep -n -i '"id": "sdl' ci/dependencies.json` finds nothing). So widths may differ by hinting and advance
rounding, by an amount UNVERIFIED until PR12 measures it; either way the TV's compile is the authority.

## Captions: the band a HUD occupies

- The band is DERIVED from the compiled geometry, not declared: `controls_top_y` = the top of the highest
  drawn INTERACTIVE extent (a disc or pill, the bar and its knob, Up Next's two buttons, the Skip pill; Up Next's still
  and caption are decor, "Only the two BUTTONS are pointer targets", `up_next.rs:268`) in the compact state,
  over its three slot states; under `band: "block"` it is the compact block's top over the three slot states, decor
  included, an Up Next column's overhang too (the reference uses it so a lifted cue clears its title; its Up Next draws
  only the two buttons, so its compact top is the same in every slot). `ceil_y = controls_top_y - CAPTION_INK_CLEARANCE`
  (12 px, classic's own 792 - 780; the lifted ink overhangs the ceiling by about 2 px, `text_style.rs:44-46`),
  `lift = SUB_BASE_Y - ceil_y`. For classic this is 792, 780 and 200 exactly (F28): its buttons sit at `CTRL_Y` 792 in every slot, and a lifted cue
  may overlap the Up Next still and caption above them, as the fixed 780 allows today. Neither name matches the
  `MAX_*` pattern of `ci/list-caps.ini`.
- Bound: `controls_top_y >= MIN_CONTROLS_TOP_Y`, the highest top for which a caption block of
  `CAPTION_ANCHOR_LINES` lines at the largest `SubtitleSize` and the highest `SubtitlePosition` still fits
  between `SAFE`'s top and the lifted baseline, computed by a plain fn over `player_hud::caption_layout`
  (`:246`, made `pub(crate)`), `size_scale` and `position_lift`, pinned by a unit test; not a lazy static
  (a new global, F31). Rough arithmetic from F28 (UNVERIFIED until the fn computes it): a three-line Huge
  block is about 3 x 48 x 1.8 = 259 px, so the ceiling must sit at or below about 54 + 259 + 320 = 633 and
  `controls_top_y` at about 645. Classic's compact top 792 passes. Classic's card (668) and strip (684)
  would pass too, being BELOW 645; only the reference's expanded block (top near y 480 in its chapters plus
  info state, an estimate) would fail, and under D8 the expanded states do not define the band, so no lift
  clamp is needed in 1.0.
- Plumbing: `text_style::HudBand { ceil_y, controls_top_y, lift }` with `HudBand::CLASSIC` equal to today's
  constants; every F28 reader takes the band as an argument (`caption_geometry_for`, `hud_lift`/`overhangs`,
  `today_baseline`, the libass lift, the synth test); the player screen passes `HudGeometry::band()` down, so
  media still names nothing above it. The tie test runs for `CLASSIC` and for a non-classic band.
- Decor may overlap captions, as classic's title does today; registry CI renders captions at the largest
  size so a reviewer sees it.

## Menus and other HUD popovers

Users may put buttons anywhere, so a menu cannot keep classic's fixed corner (F20). Placement follows the
opener, through ONE shared placer.

- The placer. `ui/src/popover.rs` gains `anchor_panel(anchor: Rect, size: (w, h), need_h: f32, gap: f32,
  how: Placement) -> Anchored { rect, side }`. `Placement` carries the caller's side ORDER (a list of `Below`, `Above`,
  `Start`, `End`, or the `Facing` order below), its CROSS-AXIS rule (`Leading`: the anchor's left edge, `under_or_above`'s;
  `Hang`: `hang_x`'s half rule), its OVERFLOW rule (`ClampTop`: `under_or_above`'s `.max(PANEL_EDGE)`, which may cover a
  tall list's opener; `ClampToRoom`: never over it) and its `keep` rect. `under_or_above` becomes a call of it with
  `[Below, Above]`, `Leading`, `ClampTop` and its `PANEL_EDGE`/`PANEL_EDGE_X` frame, keeping its width and height clamps
  (`popover.rs:283-301`), so Also available and Play Version (`alt_sources.rs:410`, `versions_panel.rs:95`) get the SAME
  rects, pinned by `alt_sources_tests.rs:674-736` (`r.x == btn.x` at `:681`, the tall case at `:700-701`); `hang_x` stays
  the function its two callers (`watchlist/options.rs:317`, `library/menu.rs:470`) call, untouched. No existing caller's
  result changes and none of the four files is edited. The player's menus pass `Facing`, `Hang`, `ClampToRoom` and
  `keep = SAFE`. `Facing`: the vertical side facing `keep`'s centre (Above for an opener in the lower half), the other
  vertical side, the horizontal side facing the centre (Start for an opener in the right half), the other. Room: for
  Above/Below, the HEIGHT between the anchor's edge plus `gap` and `keep`'s edge; for Start/End, the WIDTH between the
  anchor's edge plus `gap` and `keep`'s edge, which must hold `w`, with `keep`'s height for `need_h` (the panel centred on
  the anchor vertically, clamped into `keep`). The first side whose room holds the panel wins; if none does, the vertical
  side with the most room wins and the height is clamped to it (the `TableView` scrolls, as a long dub list does today,
  `track_menu.rs:341-345`). Cross axis for Above/Below: `hang_x` into `keep`.
- The anchor. The opener's REST rect from `HudGeometry` (not its popped rect, so the `CtlPop` never moves
  a panel), widened for the cross axis to its SEGMENT's outer edge when the segment is start- or
  end-aligned. So openers that sit together share an edge by construction, and classic, whose one
  end-aligned segment ends at `CTRL_RIGHT`, gets `CTRL_RIGHT - pw` for both menus, today's numbers (F20).
  The anchor is latched when the menu opens: a stand-in that takes the row while the menu is up, or a
  language change, does not move an open panel. A menu opened by something that is not a HUD control has
  no HUD anchor, and takes the root state's `open_more` rest rect as its anchor: the failure read-out's
  `More { page: Some(Quality) }` (F18, formerly `More { quality: true }`) therefore opens where classic opens it today, `CTRL_RIGHT - pw`, bottom 764, and a
  headless trigger is the same `Opener::NONE` case (`popover.rs:321`).
- The gap. `theme::layout::PLAYER_MENU_GAP = 28.0`, named (it is 792 - 764 today, F20, and `PANEL_GAP` is
  24), replaces `PLAYER_MENU_BOTTOM`; classic's bottom edge stays 764.
- The need. `need_h` is the tallest page the menu can show with app-owned rows (More: the Quality page with
  Auto, about 600 px per `more_menu.rs:592-602`; Tracks: its tallest Style page), measured in every shipped
  language; a server-length list (audio and subtitle tracks) may exceed it and scrolls. Classic's room above
  its row is 764 - 54 = 710. The track menu's cap moves from `bottom - 60` (704) to the room (710), and
  More's from `bottom * 0.86` (657) to 710: settled classic frames change only when a list is taller than
  the old cap, which the More menu's tallest page (about 600) is not.
- Motion and scrim stay app-owned and unchanged. Both menus keep `OpenStyle::Appear` (switching to `Morph`
  would be a visible change, and these panels stand over the video plane, not on glass); the container's
  appear spring (`K_APPEAR`, `popover.rs:84`) and `PanelMotion`'s resize springs are untouched; the rise now
  starts displaced toward the opener's side (from below for Above, from above for Below, sideways for
  Start/End), which is what "the morph origin is the opener" means for `Appear` (`Popover`'s rise is vertical only today, `popover.rs:653-656`; it gains a direction in place, every
  existing caller passing its vertical one, so no existing frame changes), and More's literal `16.0`
  becomes `Popover::RISE` like the track menu's (a 4 px change in the rise only). The scrims stay per kind,
  `DIM_PLAYER` and `DIM_SHEET` (F18); a package cannot change them, and the opener is not lifted above the
  dim (as today).
- Invariants (lint and a census test over every package, every state and every shipped language): M1 a menu
  never intersects its own opener's rest or popped rect; M2 it lies inside `SAFE` with its full `need_h`
  (no app-owned row scrolls); M3 the rise originates on the opener's side; M4 openers adjacent in one
  start- or end-aligned segment share the panel's near edge; M5 the overscan audit's menu rects come from
  the placer, not from constants (F20). The reference needs the Start side: in its chapters plus info state
  the row sits near y 590 (estimate), so neither 508 px above nor about 350 below holds the More menu's 600,
  and the panel opens to the left of the gear.
- Other HUD popovers (`OverlayKind::ALL`, F18, plus what the HUD floats):
  - Timing capsule: hides the whole HUD (`HudPolicy::Hidden`) and sits at the caption band, 836 (F22); a
    package never sees a frame it is up. Unchanged.
  - Info and Chapters: no longer popovers; they become sections (D6) and, as today, draw no scrim (F18).
  - Scrub-preview card: it already takes the bar's geometry as arguments (F22), so it follows the
    package's bar; lint requires its top to stay inside `SAFE` in every state that draws the bar.
  - Failure read-out and repair alert: own the frame or their group (F1, F22); unchanged.
  - Hold hint: none is wired in the player (F22).
- Up Next and Skip with menus: a menu is an overlay, so the countdown cannot run while it is open (F19); a
  stand-in may take the row under an open menu, as today (the row draws under Tracks and More, F2), and the
  latched anchor keeps the panel still. With sections: D7.

## Reading the reference image

`ref.png` is 1672x941, three stacked crops of one HUD. If each crop shows the full 1920 canvas it is at
0.871 scale and the bar's ends sit about 140 logical px from the edges (an assumption; the crops' vertical
placement is unknown). Under that scale the mock's type is 14-30 logical px, below the app's ladder, so the
package is built at couch-legible rungs and its expanded states are taller than the mock.
- Compact: kicker "S2, E3 · Laura" (grey, regular), title "The Morning Show" (white, bold), a gold bar with a
  white knob at rest, "5:27" at the bar's left end and "-52:33" at its right end; one row of bare white
  glyphs: [subtitles, back 10, previous, a larger solid white play/pause disc, next, forward 30] centred on
  the screen, and [chapters list, info, gear] ending at the bar's right end.
- Chapters: below the row, a "Chapters" heading, then eight cards (thumbnail, start time, name; the first
  ringed, with an equalizer glyph). The block is taller and its top moved up: bottom-anchored.
- Chapters plus info: below the strip a full-width hairline and an info card (still, kicker, title, a
  two-line synopsis, a vertical hairline, "56 min · 2021 · TV-MA · Drama"); its cards are shorter.
- The heading is not placed alike in the two crops: in the chapters crop it clearly sits on its own line
  under the row; in the third it sits just under the row's glyphs with less air. The package puts it in its
  own band in both, with that band's gap keyed on `panel.info`.
- Absent: an audio disc, tab pills, a state glyph beside the clock, info-card buttons, badges.

## Worked example: `reference.plxhud.json`

Strict JSON, so no comments; the tables after it explain every element. Asset path data is shortened to small
valid marks (the numerals stand in as bars); a published package carries full outlines. Sizes follow the
image's proportions rounded to tokens and are approximate.

```json
{
  "$schema": "https://plxnative.com/hud/schema/1.0.json",
  "format": "1.0",
  "id": "com.plxnative.examples.reference",
  "version": "1.0.0",
  "name": "Reference",
  "author": "PlxNative",
  "license": "CC0-1.0",
  "palette": { "gold": "#f2b535" },
  "assets": {
    "subs-bubble": "<svg viewBox='0 0 24 24'><path d='M3 4h18v12H10l-5 4v-4H3z'/></svg>",
    "list": "<svg viewBox='0 0 24 24'><path d='M4 6h16v2H4zM4 11h16v2H4zM4 16h16v2H4z'/></svg>",
    "gear": "<svg viewBox='0 0 24 24'><path d='M10 2h4l1 3 3-1 2 3-2 2v2l2 2-2 3-3-1-1 3h-4l-1-3-3 1-2-3 2-2v-2L4 7l2-3 3 1z'/></svg>",
    "eq": "<svg viewBox='0 0 24 24'><path d='M6 14h2v6H6zM11 8h2v12h-2zM16 11h2v9h-2z'/></svg>",
    "back10": "<svg viewBox='0 0 24 24'><path d='M12 1v2a9 9 0 1 1-9 9h2a7 7 0 1 0 7-7v3l-4-4zM9 8h1.5v8h-1.5v-6.5l-1.5 1.5v-1.5zM13.5 8h2l1.5 1.5v5l-1.5 1.5h-2l-1.5-1.5v-5zM13.5 10v4l0.5 0.5h1l0.5-0.5v-4l-0.5-0.5h-1z'/></svg>",
    "fwd30": "<svg viewBox='0 0 24 24'><path d='M12 1v2a9 9 0 1 0 9 9h-2a7 7 0 1 1-7-7v3l4-4zM8 8h2l1 1v6l-1 1h-2l-1-1v-0.5h2.5v-2h-2v-1.5h2v-1.5h-2.5v-0.5zM13.5 8h2l1.5 1.5v5l-1.5 1.5h-2l-1.5-1.5v-5zM13.5 10v4l0.5 0.5h1l0.5-0.5v-4l-0.5-0.5h-1z'/></svg>"
  },
  "layout": {
    "anchor": "bottom",
    "inset": "MD",
    "x_inset": "LG",
    "band": "block",
    "sections": "inline",
    "scrim": { "cover": "block", "extra": 120 },
    "block": [
      { "id": "title", "cells": [
        { "align": "start", "vstack": { "gap": "XS", "w": "fill", "cells": [
          { "text": { "bind": "title.kicker", "size": "CAPTION", "ink": "TEXT_SECONDARY", "bold": false, "max_lines": 1, "w": "fill" } },
          { "text": { "bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill" } }
        ] } }
      ] },
      { "id": "bar", "gap_before": "SM", "cells": [
        { "align": "start", "playbar": { "w": "fill", "fill": "gold", "track": "RAIL_TRACK", "thickness": 6,
          "knob": "always", "clocks": "ends", "clock_size": "CAPTION", "mark": "busy_only" } }
      ] },
      { "id": "row", "gap_before": "XS", "z": 1, "cells": [
        { "align": "center", "segment": { "id": "center", "gap": "LG", "items": [
          { "id": "subs", "command": "open_tracks", "tab": "subtitles", "widget": "disc", "size": 52,
            "rest": "bare", "focus": "accent", "face": "subs-bubble" },
          { "id": "back", "command": "seek", "by": -10, "widget": "disc", "size": 52,
            "rest": "bare", "focus": "accent", "face": "back10" },
          { "id": "prev", "command": "previous", "widget": "disc", "size": 52, "rest": "bare", "focus": "accent",
            "face": { "default": "builtin:previous", "restart": "builtin:restart" } },
          { "id": "play", "command": "toggle_play", "widget": "disc", "size": 64, "rest": "solid", "focus": "ring",
            "face": { "playing": "builtin:pause", "paused": "builtin:play" } },
          { "id": "next", "command": "next", "widget": "disc", "size": 52, "rest": "bare", "focus": "accent",
            "face": "builtin:next" },
          { "id": "fwd", "command": "seek", "by": 30, "widget": "disc", "size": 52,
            "rest": "bare", "focus": "accent", "face": "fwd30" }
        ] } },
        { "align": "end", "segment": { "id": "right", "gap": "MD", "items": [
          { "id": "chapters", "command": "toggle_section", "section": "chapters", "widget": "disc", "size": 52,
            "rest": "bare", "focus": "accent", "face": "list" },
          { "id": "info", "command": "toggle_section", "section": "info", "widget": "disc", "size": 52,
            "rest": "bare", "focus": "accent", "face": "builtin:info" },
          { "id": "more", "command": "open_more", "widget": "disc", "size": 52,
            "rest": "bare", "focus": "accent", "face": "gear" }
        ] } }
      ] },
      { "id": "chapters_head", "when": "panel.chapters", "gap_before": { "value": "SM", "if": { "panel.info": "XS" } }, "cells": [
        { "align": "start", "text": { "bind": "label.chapters", "size": "LABEL", "ink": "TEXT_PRIMARY", "bold": false } }
      ] },
      { "id": "chapters", "when": "panel.chapters", "gap_before": "XS", "cells": [
        { "align": "start", "strip": { "bind": "chapters", "w": "fill", "card_w": 192,
          "card_h": { "value": 108, "if": { "panel.info": 84 } }, "gap": "SM", "radius": 8, "focus": "ring",
          "card": {
            "art": { "bind": "item.thumb", "crop": "cover" },
            "overlay": [ { "when": "item.is_current", "align": "center", "valign": "center",
                           "icon": { "asset": "eq", "size": 28, "ink": "TEXT_PRIMARY" } } ],
            "below_gap": "XS",
            "below": [
              { "text": { "bind": "item.start", "size": "MICRO", "ink": "TEXT_SECONDARY", "bold": false } },
              { "text": { "bind": "item.title", "size": "CAPTION", "bold": false, "max_lines": 1, "w": "fill",
                          "ink": { "value": "TEXT_SECONDARY", "if": { "item.focused": "TEXT_PRIMARY" } } } }
            ] } } }
      ] },
      { "id": "info_rule", "when": "panel.info", "gap_before": "SM", "cells": [
        { "align": "start", "rule": { "axis": "h", "length": "fill", "ink": "HAIRLINE" } }
      ] },
      { "id": "info", "when": "panel.info", "gap_before": "SM", "gap": "LG", "cells": [
        { "align": "start", "image": { "bind": "info.still", "w": 292, "h": 164, "radius": 8, "crop": "cover" } },
        { "align": "start", "valign": "top", "vstack": { "gap": "XS", "w": 760, "cells": [
          { "text": { "bind": "title.kicker", "size": "CAPTION", "ink": "TEXT_SECONDARY", "bold": false, "max_lines": 1, "w": "fill" } },
          { "text": { "bind": "title.main", "size": "HEADLINE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill" } },
          { "text": { "bind": "info.summary", "size": "CAPTION", "ink": "TEXT_SECONDARY", "bold": false, "max_lines": 2, "w": "fill" } }
        ] } },
        { "align": "end", "valign": "center", "rule": { "axis": "v", "length": 80, "ink": "HAIRLINE" } },
        { "align": "end", "valign": "center", "facts": { "bind": "info.facts",
          "order": [ "duration", "year", "content_rating", "genres" ], "genres_max": 1, "bold": false,
          "playback_fact": false, "badges": [], "size": "CAPTION", "ink": "TEXT_SECONDARY" } },
        { "align": "end", "spacer": { "w": 150 } }
      ] }
    ],
    "up_next": {
      "replaces": [ "center", "right" ],
      "band": "row",
      "align": "center",
      "still": false,
      "caption": false,
      "watch_credits": { "rest": "film", "focus": "accent" },
      "play_next": { "rest": "solid", "focus": "ring" },
      "countdown": "ring"
    }
  }
}
```

Choices and why: Up Next replaces both segments, because two labelled buttons beside the centred cluster
leave almost no room in English at these widths (estimate) and none in longer languages; a wider design can
name one segment. It draws only the two buttons (`still: false`, `caption: false`): a centred, two-segment replace has no
floor, so the primary is its own label's width, but any still or caption would overhang the bar and title just above
the row, which I4 refuses; a design that wants them gives the row a band with room above it. `band: "block"` keeps
lifted captions off the title. The heading and the chapter start
time hug (app text, D10); the chapter name is server text and may elide on unfocused cards, marquee on the
focused one (D11). Estimated compact block height about 210 px, so its top lands near y 790, below the
caption bound's estimate (645); the chapters plus info block is about 520 px with its top near 480 (all
estimates at ladder line heights, UNVERIFIED; the golden `reference_passes_every_invariant` replaces them
with computed values). The end-aligned `spacer` of 150 reproduces the image's gap after the facts (the run ends near
1422 / 0.871 = 1633 against the content edge 1784; an estimate).

### Image element -> format feature

| # | Element (crops) | Format feature | Drawn by |
|---|---|---|---|
| 1 | Dark bottom gradient (1-3) | `scrim {cover: block, extra}` | `draw_scrim` |
| 2 | Kicker "S2, E3 · Laura" | `text title.kicker`, CAPTION, regular; only the episode title may elide (D10) | `p.text` after `fit_line` |
| 3 | Title "The Morning Show" | `text title.main`, TITLE, bold | the title block; the paused reveal stays app-owned |
| 4 | Gold played part | `playbar.fill: "gold"` (palette, D12) | `Playbar` fill field |
| 5 | Grey unplayed track | `playbar.track: RAIL_TRACK` | `Playbar` |
| 6 | White knob at rest | `playbar.knob: always` | `Playbar` |
| 7-8 | "5:27" left, "-52:33" right | `playbar.clocks: ends`; `fmt::clock`, minus from `fmt_time` (F27) | `draw_clock` |
| 9 | Speech bubble | `open_tracks {tab: subtitles}`, `rest: bare`; the menu reaches Audio (F15) | `TransportButton` |
| 10 | Arrow "10" | `seek {by: -10}`, static face | `TransportButton`; the extracted jump |
| 11 | Previous | `previous`, `{default, restart}` faces | `TransportButton` |
| 12 | Large white disc | `toggle_play`, size 64, `rest: solid`, `focus: ring`, `{playing, paused}` | `TransportButton`; the extracted toggle |
| 13 | Next | `next` | `TransportButton` |
| 14 | Arrow "30" | `seek {by: 30}`, static face | `TransportButton` |
| 15 | Cluster centred | segment `center`, `align: center` | `HStack` `Align::Centre` |
| 16-18 | List, (i), gear | `toggle_section {chapters}`, `toggle_section {info}`, `open_more` | `TransportButton` |
| 19 | Cluster at the bar's end | segment `right`, `align: end` | `HStack` `Align::Trailing` |
| 20 | "Chapters" heading | its own band, `text label.chapters`, hugging (D10) | `p.text` |
| 21 | Thumbnails | `strip`, `card.art item.thumb`, cover crop | `cards::Shelf` (D11), image cache |
| 22 | Ring on the first card | `strip.focus: ring`; the seat on the current chapter is app-owned (D6) | the `RowStyle` over-video rim (D11) |
| 23 | Equalizer glyph | `card.overlay` icon `eq`, `when: item.is_current` (no built-in, F36) | `icons::draw` with `IconRef::Pack` |
| 24 | "00:00" | `item.start`, MICRO; the app prints "0:00" (F27, deliberate: one clock format) | `p.text` |
| 25 | "Opening" | `item.title`, ink keyed on `item.focused` | Shelf label / overlay |
| 26 | More cards than fit | app-owned scroll and cull | `cards::Shelf` |
| 27 | Block taller, top higher | bottom anchor; bands with `when`; the rise is an app-owned spring | `VStack` `Anchor::Bottom` |
| 28 | Shorter cards in crop 3 | `card_h {value, if: {panel.info: ..}}` | compiler |
| 29 | Full-width hairline | `rule {axis: h, length: fill}` | `hairline` with an axis |
| 30 | Info still | `image info.still`, cover crop | `resolve_tex_wh_on`, `cover_uv` |
| 31-33 | Info kicker, title, synopsis | `text` bindings; synopsis `max_lines: 2` | `p.text`, `TextView::max_lines` |
| 34 | Vertical hairline | `rule {axis: v}` | `hairline` with an axis |
| 35 | "56 min · 2021 · TV-MA · Drama" | `facts` order `[duration, year, content_rating, genres]`, `genres_max 1`, regular; the app prints "56m" (F27), and the rating, a badge in classic (F23), is a run part here | `DottedRun` through `ChipRow` |

Expressed but not visible: the Up Next offer, the Skip pill (app-owned: its own label's width (no floor: the reference replaces two segments), at the stand-in's aligned edge), the busy
spinner (`mark: busy_only`), a disabled Previous (app-owned), the failure read-out, the scrub-preview still,
the track menu (from the bubble) and the More menu (from the gear, opening to the Start side when the
expanded block leaves no vertical room). Not reproduced: the mock's sub-ladder type sizes (deliberate); its
"56 min" and "00:00" spellings (the app's one format, F27); an animated equalizer (static in 1.0, motion is
app-owned); a top placement (1.1, D13).

### Classic, element by element

The format must express today's HUD exactly; the golden `classic_is_pixel_identical` (sim captures over the full state
matrix) is the proof. `ui/src/CLAUDE.md:448`, idiom 3 ("The default player HUD gets a state glyph, not a transport row"),
binds `builtin:classic`, which keeps it; a package's transport row is the owner's explicit ask (the reference image), and
a package is never the default.

Classic's compact state, tabs, scrim and Up Next, as the `layout` of `classic.plxhud.json` (every number is a constant
cited in the table below; faces are omitted, so each control draws its command's classic glyph; the file in the repo
(`appkit/src/hud_format/fixtures/classic.plxhud.json`) already has the two section bands of the table and the tabs
band's keyed `gap_before` (its chapters value is a placeholder, below), and PR6's `--hud-export builtin:classic` prints the
whole file):

```json
{
  "anchor": "bottom", "inset": 13, "x_inset": 0, "band": "controls", "sections": "modal",
  "scrim": { "cover": "fixed", "height": 470 },
  "block": [
    { "id": "top", "when": "!sections", "gap": "MD", "cells": [
      { "align": "start", "valign": "bottom", "vstack": { "gap": 0, "w": "fill", "cells": [
        { "text": { "bind": "title.kicker", "size": "CAPTION", "leading": 34, "max_lines": 1, "w": "fill",
                    "ink": { "value": "TEXT_SECONDARY", "if": { "title.is_episode": "TEXT_PRIMARY" } },
                    "bold": { "value": false, "if": { "title.is_episode": true } } } },
        { "text": { "bind": "title.main", "size": "HUD_TITLE", "leading": 54, "ink": "TEXT_PRIMARY", "bold": true,
                    "max_lines": 1, "w": "fill" } }
      ] } },
      { "align": "end", "valign": "bottom", "segment": { "id": "row", "gap": 20, "items": [
        { "id": "prev", "command": "previous", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" },
        { "id": "next", "command": "next", "gap_after": 40, "widget": "disc", "size": 64, "rest": "film", "focus": "accent" },
        { "id": "subs", "command": "open_tracks", "tab": "subtitles", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" },
        { "id": "audio", "command": "open_tracks", "tab": "audio", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" },
        { "id": "more", "command": "open_more", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" }
      ] } }
    ] },
    { "id": "bar", "when": "!sections", "chrome": "steady", "gap_before": 26, "cells": [
      { "align": "start", "playbar": { "w": "fill", "fill": "TEXT_PRIMARY", "track": "RAIL_TRACK", "thickness": 8, "clock_size": "CAPTION" } }
    ] },
    { "id": "tabs", "gap_before": 7, "cells": [
      { "align": "start", "segment": { "id": "tabs", "gap": "SM", "items": [
        { "id": "tab_info", "command": "toggle_section", "section": "info", "widget": "pill", "size": 64 },
        { "id": "tab_chapters", "command": "toggle_section", "section": "chapters", "when": "chapters.present", "widget": "pill", "size": 64 }
      ] } }
    ] }
  ],
  "up_next": { "replaces": [ "row" ], "band": "top", "align": "end", "still": true, "caption": true,
               "watch_credits": { "rest": "film", "focus": "accent" }, "play_next": { "rest": "film", "focus": "accent" },
               "countdown": "fill_sweep" }
}
```

The vertical chain, bottom up: `SAFE`'s bottom 1026 (`MARGIN_Y` 54, `ui/src/consts.rs:41`) minus `inset` 13 is the tabs'
foot 1013 = `tab_y()` 949 + `BTN_S` 64: `tab_y` = (`SB_Y` + `SCR_H`) / 2 - `BTN_S` / 2 = (882 + 1080) / 2 - 32
(`player_hud.rs:1440-1442`), and the pills are drawn and hit-tested `BTN_S` tall (`:2174-2176`, `:1424-1428`; `TabPill`
paints its whole rect, `ui/src/widgets.rs:5536-5555`), not `TAB_PILL_H`; 949 less 59 is the track's foot 890 = `SB_Y`
882 + 8; 882 less 26 is the row's foot 856 = `CTRL_Y` 792 + 64, which the title's 34 + 54 boxes share, putting the kicker
at 768 and the title at 802.

| Classic element | Today (evidence) | Keys |
|---|---|---|
| Row spacing | 20 / 40 / 20 / 20: `disc_items` gives Next `after(2 x CONTROL_GAP)` (`player_hud.rs:724-727`) on a `CONTROL_GAP` stack (`:684-686`) | segment `gap: 20`; Next `gap_after: 40` |
| Title width | `title_right`: the slot's leftmost item at `max(scale, CTRL_FOCUS_SCALE)`, under Up Next also the still and caption, minus MD (`:1769-1781`) | one band `top` with the title `vstack` and the row, `gap: MD`, `w: fill` (the Fill rule) |
| Kicker style | Episode bold primary, Context regular secondary (`:1693-1701`, `:1724-1727`), gated on `is_real_episode` (`:2097`) | state-keyed `ink` and `bold` on `title.is_episode`, defined as `is_real_episode` |
| Kicker, title y | `SCR_H - 312` and `SCR_H - 278` (`:1732`, `:1763`) | `leading` 34 and 54, `valign: bottom` with the row |
| Row | five 64 discs at `CTRL_Y` 792, ending at `CTRL_RIGHT` = `SAFE`'s right (`:476-491`) | end-aligned segment, `x_inset: 0` |
| Scrim | fixed `SCRIM_H` 470 in every state, before `if transport` (`:941`, `:1678-1686`, `:2074`) | `scrim {cover: fixed, height: 470}` |
| Chrome fade | title, row and tabs on `pc = p.alpha(chrome_a)`; bar, clocks and preview on `p` (`:2075-2081`, `:2146-2160`) | bands `top` and `tabs` default `fades`; `bar` `steady` |
| Playbar | `SB_Y` 882, 8 tall (`:474-475`); primary fill, tick knob, clocks follow the head, state glyph (F37) | `playbar` with the four keys the table requires (`fill` `TEXT_PRIMARY`, `track` `RAIL_TRACK`, `thickness` 8, `clock_size` `CAPTION`, `draw_playbar`) and the rest at its defaults, which are classic's |
| Grab band | `scrub_hit_rect` = `(SB_X, SCR_H - 270, sb_w, 160)` (`:1405-1407`) | `playbar.grab` defaults 72 / 80 |
| Tabs | Info, then Chapters only with chapters (`tab_labels`, `:1430-1435`); `SM` apart from `SB_X` (`:1457-1459`); selected while open (`hud_tab_state`, `:1587-1590`); drawn in every state, outside `if transport` (`:2160`); `BTN_S` tall at `tab_y()` 949 (`:1440-1442`, `:2174-2176`) | band `tabs` with no `when`, `gap_before: 7` (`tab_y()` 949 less the bar cell's foot, 882 + 60: the cell holds the clocks' line); `toggle_section` pills of `size: 64`, Chapters `when: chapters.present`; the selected-face rule |
| Up Next | the app-drawn column (`up_next.rs:194-200`, `:226-266`, `:290-330`), the primary floored at `ctrl_row_w` (`player_hud.rs:717-719`) | `up_next {replaces: [row], band: top, align: end, still, caption, countdown: fill_sweep}`; inside app-owned |
| Skip | `ctrl_slot` of its own label (`skip_pill.rs:75-77`) | the Skip rule (Package format) |
| Sections | modal; title, bar and row hidden while open (F2) | `sections: modal`; `when: "!sections"` on `top` and `bar` |
| Chapters strip | 288 x 162 at `CH_TOP` 684, gap 24, radius 10, ring 2.5; name 26 below the card, time 34 below the name, both at the card's left (`chapters_panel.rs:27-40`); cards past `SAFE` culled at the screen edge (`:140-147`); an empty name draws "Chapter N" (`:176-179`) | band `chapters`, `when: panel.chapters`: `strip {card_w: 288, card_h: 162, gap: 24, radius: 10, focus: ring, label_align: leading, below_gap: 26, below: [item.title LABEL bold leading 34, item.start CAPTION]}`; the tabs band's `gap_before` keyed on `panel.chapters` so the card top stays 684 (the fixture's value, 43 = 949 - 684 - (162 + 26 + 34), takes the `item.start` line as zero-height as `chapters_panel::label_ys` does; a one-line text's box is its rung's line height, so PR8 recomputes it from the compiled strip box and the fixture's number is a placeholder until then); scroll and marquee per D11 |
| Info card | ground x 80, w 1760, h 236, top `SCR_H - 176 - 236` = 668, pad 28 (`info_panel.rs:581-588`); still radius 16 (`:216`); text 34 from the still and from the buttons (`:252-255`); title to synopsis 6, tags `before(12)` (`:436-446`); synopsis `leading(29)` (`:386-389`); buttons a column 70 tall, 16 apart, centred, width max(352, widest of all three labels) (`:594-608`), right edge 1812 inside `SAFE`'s 1824 | band `info`, `when: panel.info`: `rect {bleed: 16, pad: 28, h: 236, gap: 34, ink: PANEL_TOP, radius: 28}` holding `image info.still` 320 x 180 radius 16; a `vstack {gap: 6, w: fill, valign: center}` of the title (`max_lines: 1`), the synopsis (`leading: 29`, `max_lines: 2`) and `facts` with `gap_before: 12` (`order: [genres, year, duration]`, `genres_max: 2`, `bold: true`, `ink: TEXT_PRIMARY`, `playback_fact: true`, `badges: [content_rating, audio_format, cc, sdh, ad]`); `actions {axis: column, gap: 16, item_h: 70, min_w: 352, items: [from_beginning, go_to_detail]}`, `align: end`, `valign: center`; the tabs band's `gap_before` 45 under `panel.info` (949 - 904) |
| Captions | ceiling 780 whenever the HUD is up (F28) | `band: "controls"`, interactive rects only: 792 - 12 in every slot |

## Invariants

The loader and compiler enforce these for EVERY source (built-in, registry, sideload), per state, per slot
and per shipped language (measured widths change with the language: the TV compiles for its language and
recompiles on a change; registry CI lints all eleven):
- I1. App-owned surfaces cannot be replaced or covered: the failure read-out (never red), the repair alert,
  decision prompts, caption drawing, Up Next's and the sections' logic, the Skip pill, the scrub-preview
  still, the Tracks, More and Timing overlays. `readout_owns_frame` keeps deciding whether the HUD draws.
- I2. Reachability R1-R4 under every valuation of the content conditions (`open_more` in the root; folds computed;
  openers imply sections; Up Next whole).
- I3. Identity: every `id` unique among the bands, among the segments and among the controls (three namespaces, because
  `up_next.band` and `up_next.replaces` name different kinds and classic's own `tabs` is both a band and a segment); an unknown command, parameter, binding or condition is an error; order
  is free.
- I4. z-order census: same-z interactive rects never overlap; every item hit-tests back to itself (the
  census `every_disc_hit_tests_back_to_itself`, `player_hud.rs:2500`, generalised); a lower-z group (the
  scrub band, a hit rect that is not drawn) may underlie higher ones (F8); decor of EVERY kind (text, images, icons,
  rules, Up Next's still and caption) never overlaps a drawn interactive rect in any band at any z, except that a `rect`
  ground underlies its own cells and a strip card's own `overlay` and `below` content belongs to its card (the
  reference's `eq` glyph).
- I5. Every interactive drawn and hit rect, every text, image and icon lies inside `SAFE`, except that a strip's
  cards, with their own overlay and below content, may run past `SAFE`'s side edges while the focused card is revealed
  inside it; otherwise only scrims and `rect` grounds bleed (F29). The overscan audit reads `HudGeometry` (F35).
- I6. The caption band bound (Captions) holds in the compact state under every slot.
- I7. Menus M1-M5.
- I8. Focus is visible: rest and focus faces differ.
- I9. Inks from the allow-list or the palette; no palette in `builtin:classic`; sizes are rungs or
  `HUD_TITLE` on `title.main`; weights are the two shipped.
- I10. Text: app text hugs and fits in every shipped language as the compiling host measures it; the TV's compile is
  the authority and fails per language (Text fit); server text may elide or wrap (D10).
- I11. SVG subset, every asset a mask with no gradient, colour or opacity content (Package format, SVG assets); a
  `seek {by: "setting"}` face map has `default`.
- I12. Cost per state class (below).
- I13. `layout.anchor` is `bottom`.

## Cost

A HUD's element count is an authoring budget, not content a user reaches, so the bounds are derived. The
compiler computes a cost per STATE (draw ops, glyph runs, texture bytes, and BLENDED PIXEL AREA, the summed area of
every translucent fill the state draws, scrims and grounds included, which is the fill the Mali frame-drop work measures;
the strip counted by the cards a band
can show, its width over the card pitch plus one, never by chapter count). The ceiling is per state class,
each calibrated on the TV with the `profile-tv` tools (`ARM_PROFILE=release`, an `fps_floor` scene, zero late
frames, Mali HWCNT per frame):
- compact: classic's compact HUD, and the reference's;
- one section open: classic's chapters and info states;
- two sections open: classic has none, so the reference's chapters plus info state calibrates it, in the
  sections PR. If that state does not run clean, its visible card count is cut until it does and that is the
  ceiling; no ceiling ever admits a cost no measured state reached.
A package's state cost must be at or under its class's ceiling in every term; the blended-area term, calibrated with
HWCNT like the others, is what bounds a tall scrim or a screen-wide ground, each only a few draw ops. SVG bytes and path count per asset bound the C
parser's input; package bytes follow from both. Every new `MAX_*` gets its `ci/list-caps.ini` line with the
derivation.

## Storage per install mode

New `paths::hud_candidates()` on F39's pattern, keyed by `app_id()`:
0. a steerable (hostsim) build: `in_runtime_dir("huds")` FIRST, so simulators never share one store;
1. `/media/developer/{id}-huds/` (Dev Mode: rw, survives reinstall);
2. `/media/internal/.{id}-huds/` (the HBC/cryptofs jail's persistent rw location);
3. `/media/developer/temp/{id}-huds/` if (1) is refused, created owner-only like the image cache;
4. a device build: `in_runtime_dir("huds")` LAST; it is `/tmp`, so Settings says "Installed until restart"
   when only it took the write. `in_app_dir` is not a candidate (not writable by the jailed uid, F39).

The first candidate that accepts a write is used; all are read when listing; the flavours never share (D14).
Layout: `packs/<sha256>.plxhud.json` (content-addressed, so an update is a new file and a switch is atomic),
`installed.json` (id -> sha, source, installed-at), `index-cache.json` (plus ETag), `drop/` (the sideload
drop folder, scanned when HUD settings open). UNVERIFIED: that a rooted set runs the app under one of the two
jails by prefix (F39's table does not say); if so, the same order works.

## Sideload: getting a file onto the TV

- (a) CHOSEN, every install mode: Settings > Player HUD > "Install from address", through the existing text
  input (F40). It accepts `gh:owner/repo[@tag]` (that repo's release asset `hud.plxhud.json`, 15-25 typed
  characters) or any `https://` URL, and `http://` only for RFC1918 hosts, labelled a dev source. The only
  redirect door has no body bound (F40), so the address PR adds a bounded public fetch beside
  `https_get_public`: the same headerless, redirect-following request with `Some(max_body)` through
  `request`'s existing argument; no new curl option, but a curl-path change, so `fw-compat-reviewer`.
  UNVERIFIED: whether the TV's CA set accepts `objects.githubusercontent.com`, where release redirects land.
- (b) CHOSEN, Dev Mode and rooted: copy the file into `<root>/drop/` over ssh; it is validated and listed when
  HUD settings open, and installed only when the user confirms it on the TV (`InstallDrop`, which re-reads the
  file and refuses it if it changed since the list). No network.
- (c) QR to a web page: rejected; the site is static (F40) and cannot hand a file back.
- (d) LAN page served by the app: rejected; a listening socket plus a worker (ledger growth, F31), and
  inbound reachability through the TV's firewall is UNVERIFIED.
- (e) Phone pairing through a relay: rejected; a backend storing user uploads (abuse, moderation, cost).
  `lab.plxnative.com` is diagnostics infrastructure (`src/lab/control.rs:5`) and is not proposed.
- (f) USB stick: not until the jail mount is verified on both profiles (F39).

Every sideload passes the same invariants with no review; Settings labels it "Unreviewed", and installing
it takes an explicit confirmation on the TV.

## Registry, and the Homebrew question

"A GitHub repo like Homebrew" is right for SUBMISSION and wrong for DELIVERY; Homebrew itself moved its
clients from cloning `homebrew-core` to a JSON API generated from the repo (Homebrew 4.0, 2023; UNVERIFIED in
this session, no network).
- Source of truth: a separate public repo, `plxnative-huds`, one directory per HUD (`huds/<id>/hud.plxhud.json`
  plus a README); a PR is the submission and the author's GitHub identity is the identity. Separate from the
  app repo so contributor PRs never touch app `main` (squash-only, AGENTS.md).
- CI on each PR: (1) `plxnative-sim --hud-lint`, the app's own validator in the simulator artifact pinned to
  an app release, so CI and the TV run the same compiler and rules (text WIDTHS can still differ, and the TV's compile
  decides, Text fit); (2) previews from the Linux simulator under xvfb (F34)
  over a fixed matrix: playing, paused, seeking, an episode with Previous/Next, Up Next, Skip Intro, every
  section state the package allows, both menus open from every opener, a failure read-out, long titles in
  each shipped language, captions at the largest size and highest position with the HUD up, bright and dark
  mock frames; posted on the PR; (3) a diff against the previous version's previews.
- Re-lint on every app release, with `unfit`, `app.max` and an issue for the author (Text fit, (a)).
- Publish on merge: a static `index.json` (`id`, `version`, `format`, `app` range, `sha256`, `bytes`, preview
  URLs, author, license, `unfit` per Text fit), packages at `p/<sha256>.plxhud.json`, a `revoked` list of hashes with reasons, and
  `index.json.sig` (PR14). Hosted on that repo's Pages under a plxnative.com subdomain (UNVERIFIED: DNS), or
  under this repo's `site/` if the owner prefers one Pages site.
- TV side: one conditional GET of the index (ETag) that does NOT follow redirects, because a
  redirect-following request may carry no headers (F40); the URL is pinned to the host that serves it, and a
  3xx is an error. It refreshes only when HUD settings open, at most once per 24 h, never at boot or during
  playback, with backoff. The 24 h rule lives in the HudStore machine, which reads no clock (F31): the runner
  hands `now` in on the event that opens HUD settings (PR13 adds the field to `Refresh`). A package is fetched by hash and checked with `plx_base::sha256` before
  it is written. Offline, installed HUDs keep working; the gallery shows the cached index with its date.
- Why: TVs reading GitHub directly hit API rate limits (UNVERIFIED figure: 60 unauthenticated requests per
  hour per IP) and need tree walks; a bespoke backend means hosting and moderation for no gain over static
  files; content addressing makes caching, revocation and "the preview is this exact file" checkable.
- Registry approval tier (human review plus CI): no red or alarm treatment, legible over the brightest and
  darkest frames CI renders, no Plex or LG marks or impersonation (`docs/distribution.md` section 5.5), a
  redistributable license, previews that match the hash.

## Trust and safety

- Tiers: Built-in, Registry (CI plus review), Unreviewed (sideload, `gh:`, URL). Every tier passes the format
  invariants; only the registry tier is reviewed.
- Integrity: every package is identified by its sha256, and the index names hashes, so a mirror cannot
  substitute content. Authenticity: HTTPS first; PR14 adds a detached ed25519 signature over the index, checked
  against a public key compiled in, through either a small verify-only crate (a Cargo change, so a
  `make build-bench` table) or OpenSSL EVP through the TV's libcrypto (FFI, so `fw-compat-reviewer`;
  UNVERIFIED whether reachable). The gallery is on by default in a stable release only after PR14.
- Revocation: a revoked hash disables the matching package from every source, sideloads of the same bytes
  included; the fallback is `builtin:classic` and Settings says why. It does not stop an author who changes a
  byte; the invariants are the protection there.
- What a package can do is bounded by the command vocabulary: no account write, no setting change, no network,
  no delete, no macro (Commands). Attack surface: the strict JSON parser and the SVG subset check (Rust) before
  nanosvg (C), and the compiler (Rust, bounded by the cost model). The format PR ships in-crate seeded mutation
  tests over `parse + compile` and the SVG checker; a `cargo fuzz` package is PR16.
- Privacy: a registry fetch reveals the TV's IP to the host, like any web fetch; no account or identifier is
  sent.

## LLM authoring

Decision: the format is written for models by four supports: a published schema generated from the parser's
own table, a lint whose every finding names its fix, published examples and a guide, and a vocabulary with one
spelling per concept and no expressions. Goal: a model given the guide, the schema and one example passes
`--hud-lint` on the first or second try. External claims here are UNVERIFIED in this session (no network).

1. Schema.
- Where: `https://plxnative.com/hud/schema/1.N.json`, served from this repo's `site/` (static Pages, F40), so the
  URL is app-owned and stable; the registry mirrors it under `schema/`. A package's schema-URL key names it; the
  strict parser accepts the key, the TV never fetches it, and lint checks its MAJOR.MINOR equals `format`'s.
- One file per MAJOR.MINOR, immutable once published; a minor only adds, so `1.0.json` validates every 1.0
  package forever.
- One source: `hud_format::schema` holds a descriptor table (per key: type, closed values, numeric range,
  required, state-keyable; per command: its typed parameters). The strict parser walks it and
  `plxnative-sim --hud-schema` serializes it; nothing is hand-written. The emitted file is a committed golden
  that a test regenerates and diffs. The commands are a `oneOf` keyed by `const` on `command`, each branch
  listing its parameters with `additionalProperties: false`, so a validator reports a wrong parameter at the
  item.
- Agreement: the registry CI runs a stock validator (Python `jsonschema`, which implements 2020-12, UNVERIFIED)
  over the whole fixture corpus and asserts that everything the parser accepts, the schema accepts, and that
  every key, type or value error the schema reports, the parser reports at a JSON pointer that equals or extends the schema's (a stock validator reports a bad parameter
at the ITEM, through the `oneOf`, where the parser names the key). The schema is
  a sound over-approximation: duplicate keys and the compile invariants are lint's.
- Draft 2020-12, emitted with keywords whose meaning has not changed since draft-07 (`type`, `properties`,
  `required`, `additionalProperties`, `enum`, `const`, `pattern`, `minimum`, `maximum`, single-schema `items`,
  `oneOf`), plus local `$ref` into `$defs` (the format is recursive: a `cell` holds a `vstack` or `rect` that holds
  `cell`s, which a schema cannot say without a reference) and the annotations `description`, `default` and `title`,
  which a validator ignores. No validator runs on the TV: it would be a second parser of attacker input and a new
  dependency.

2. Lint.
- `plxnative-sim --hud-lint <file> [--format text|json] [--lang all|<tag>]`; exit 0 clean, 1 findings, 2 usage.
  It runs to the end and reports EVERY finding (parse, then compile per state, slot and language), sorted by
  pointer, so one round fixes many.
- A finding: `code` (stable, never reused, listed in the guide), `severity` (`error` or `warning`; a missing
  rung of a `by: "setting"` face map is the warning), `path` (an RFC 6901 pointer), `rule` (the invariant's name
  as in this plan), `message`, `hint` (a concrete fix), `nearest` (closest valid names by edit distance over
  the closed vocabulary), and context `state`, `slot`, `lang`, `rects` where they apply. JSON output:
  `{"lint": 1, "file": ..., "findings": [...]}`. SARIF 2.1.0 was weighed and left for later: a larger surface
  for no gain over JSON, which the registry CI turns into PR annotations.
- Example: `{"code": "HUD-E0301", "path": "/layout/block/2/cells/0/segment/items/1/command", "rule":
  "command-known", "message": "unknown command skip_back", "hint": "a relative jump is the command seek with a
  signed by in seconds: by -10", "nearest": ["seek"]}`.
- Codes are grouped by hundreds per rule family. Beyond the example: `HUD-E0601` app text does not fit (context `lang`,
  the string, its measured width, the room, the rect it meets); `HUD-W0602` app text fits but inside `CI_TEXT_SKEW`
  (an error in registry CI); `HUD-E0701` an SVG element outside the mask subset (for `linearGradient`,
  `radialGradient` and `stop` the hint reads "HUD assets are alpha masks tinted by the app; a gradient would be
  discarded, so draw the mark in one colour"); `HUD-E0702` an SVG attribute outside it (`fill`, `stroke`, `opacity`,
  `style`: "colour comes from the face's ink").
- Information lines, not findings: the commands the app will fold into More, and the menu side each opener
  gets in each state.
- What JSON Schema cannot express, lint reports with numbers: the z-order census (both rects, their groups and
  z, the state and slot); the caption band (computed top, `MIN_CONTROLS_TOP_Y`, the element too high, by how
  many px); reachability (R1, R3, R4); menus (the room on each side against `need_h`); cost (per state against
  its class ceiling, the three largest contributors); the safe area, focus visibility, unused palette entries,
  decor overlap, and app text widths in every shipped language (`--lang all`).
- Foreign spellings get hints from an alias table, never accepted syntax: a template string such as
  `${title.main}` -> `"bind": "title.main"`; a Kodi info label such as `VideoPlayer.Title` -> `title.main`;
  `onPress`, `action` or `handler` -> "a press declares a command: use the command key with one of ...";
  a DTCG-shaped palette entry -> the flat hex form.

3. Examples and a guide.
- Published beside the schema (`site/hud/examples/`, mirrored in the registry's `examples/`):
  `reference.plxhud.json` (above); `classic.plxhud.json`, emitted by `--hud-export builtin:classic` from the
  embedded JSON, never copied; small variants `minimal` (the smallest valid package: a playbar and a `more`
  disc), `hero-center` (a `toggle_play` hero), `tabs` (openers as pills), `palette`. Each invalid fixture of the
  corpus is published with its lint output, so a model sees error and fix side by side. Every example is a
  golden linted in CI, so none can rot.
- `AUTHORING.md` (one page, for humans and models): the mental model (bands, cells, segments, controls with an
  id and a command, sections, states), each closed vocabulary in one line, every invariant with its lint code,
  a writing order (skeleton, rows, sections, palette last), and the lint loop.
- Registry entry points: `AGENTS.md` (run lint, read `AUTHORING.md`, never hand-edit `index.json`) and
  `llms.txt` (a markdown index of schema URL, guide, examples and lint command; a 2024 proposal of uncertain
  adoption, UNVERIFIED, costing one static file). Registry authors never need the app repo.
- Acceptance measure, manual and not CI: before submissions open the owner can give a model the guide, the
  schema and one example and count lint rounds. It is nondeterministic and spends API calls, so it is no gate.

4. Conventions weighed.
- JSON Schema: ADOPTED, draft 2020-12, for publication, lint and editors only.
- W3C Design Tokens (DTCG): REJECTED for the palette. Its `$value`/`$type` objects and `{group.token}` aliases
  (UNVERIFIED in detail) would bring dollar-prefixed keys, groups and alias resolution with cycle detection into
  the TV's parser for a flat one-type list; `"gold": "#f2b535"` is the commonest colour spelling there is. Lint
  hints the flat form; an authoring tool may import DTCG.
- Adaptive Cards templating: REJECTED as syntax (its expressions run on a language with arithmetic, comparison
  and string functions, UNVERIFIED in detail: an expression language, D1). Borrowed: a repeater bound to a list
  (`strip.bind`) and a presence condition (`when`), both over closed names.
- Kodi skins: REJECTED as structure (XML, includes, window ids, a boolean condition language); its info labels
  are familiar to models, so lint carries a Kodi alias table for hints.
- DivKit: ideas only (a JSON server-driven UI with templates, typed blocks, states and a published schema, plus
  an expression language with variables, UNVERIFIED). Borrowed: a card template inside a repeater, and states.
  Rejected: expressions, variables and an open vocabulary.
- Lottie: REJECTED: a timeline format needs its own player, a second renderer (D2), and motion is app-owned (D3).
- Event handlers in general (HTML `onclick`, Adaptive Cards `Action.Execute`, DivKit actions): the useful part is
  that a press names what it does; the part that is code is dropped. A control names one command (D4).
- House style: one spelling per concept; snake_case keys and commands, UPPER_CASE tokens, lower-case palette
  names; no synonyms; no defaults that change with context; ambiguous input is an error.

## PR breakdown

Every PR runs its gates on the committed tree (AGENTS.md); "pixel-identical" means sim captures over the full
state matrix before and after, via `ui-sim`; TV work holds the TV lock around the device commands only.

| PR | Scope | Depends on |
|---|---|---|
| PR1 | Shared anchored placer in `ui/src/popover.rs`; its four callers move to it; the player menus take an anchor rect, `PLAYER_MENU_GAP`, the rise toward the opener, `Popover::RISE` for More; overscan menu rects from the placer | - |
| PR2 | Split the HUD into state, model, motion and geometry; HUD travel and OK on FocusEngine groups | - |
| PR3 | Control identity and the command vocabulary; element ranges re-cut; one shared command performer | PR2 |
| PR4 | `HudBand` as a value through every caption reader | PR2 |
| PR5 | `appkit::hud_format`: descriptor table, strict parser, SVG subset, compiler, reachability and folds, lint flag | - (reviewed with PR7) |
| PR6 | Schema emission, examples and the site copies | PR5 |
| PR7 | One renderer for the compact HUD, Up Next and Skip; `builtin:classic` pixel-identical | PR1, PR3, PR4, PR5 |
| PR8 | Chapters and Info as HUD sections; the strip on `cards::Shelf`; the overlays removed | PR7 |
| PR9 | The More fold, and `open_more {page: speed}` | PR3, PR8 |
| PR10 | HudStore machine, local install, selection, drop folder | PR8, PR9 |
| PR11 | Install from address | PR10 |
| PR12 | Registry repository and authoring kit | PR6, PR8 |
| PR13 | In-app gallery | PR10, PR11, PR12 |
| PR14 | Signed index (gates the stable default) | PR12, PR13 |
| PR15 | Authoring tools | PR6 |
| PR16 | `cargo fuzz` package | PR5 |
| PR17 | Owner-gated: lift captions clear of open sections | PR8 |

PR1. Shared menu placement.
- Files: `ui/src/popover.rs` (`anchor_panel` with a per-caller `Placement`; `under_or_above` a call of it
  with today's rules; `hang_x` its cross-axis rule; `Popover`'s rise gains a direction; the four callers' files are not
  edited),
  `appkit/src/{track_menu,more_menu}.rs` (placement from an anchor rect the player screen hands in at open,
  today the row's own rect with its segment edge `CTRL_RIGHT`), `ui/src/theme.rs` (`PLAYER_MENU_GAP`),
  `screens/src/player/overlay.rs` (hand the anchor over).
- Verify: the existing placer tests unchanged and green with identical rects (`alt_sources_tests.rs:674-736`, `popover_host_tests.rs:254-258`)
  plus new ones for each side, the flip, the room clamp and the latched anchor; classic's settled menu frames
  pixel-identical (the More rise differs by 4 px mid-animation, the caps only past the old heights, F20);
  `make check`, `make check-nodef`.
- Risks: a caller's panel moving by a pixel through the generalisation; the pins catch it.

PR2. HUD split (no behaviour change).
- Goal: `HudModel` (every F3 read presentation needs), `HudMotion`, `HudGeometry`; the player's `Focusable`,
  pointer stops, `overscan_rects` and the stand-ins read `HudGeometry`; `nav.focus` becomes a `GroupId`, Up from
  the top group hides the HUD as the screen's answer to `Step::Edge`, so a package's group order needs no new arm.
- Files: `appkit/src/player_hud.rs` -> `player_hud/{state,model,motion,geometry,classic}.rs`;
  `appkit/src/{up_next,skip_pill}.rs`; `screens/src/player/{mod,input}.rs`; `src/app/{run,playback}.rs`;
  `src/app/overscan_audit_tests.rs`; the rest of the 28 files of F35 are import moves (the list goes in the PR body).
- Verify: `make test-crate C=plx_appkit` and `C=plx_screens` (hit census included), classic's Up/Down sequences
  pinned, captures pixel-identical, `make check`, `make check-nodef`. Size by content, not diff stat (AGENTS.md).
- Risks: per-frame snapshot cost (`Copy` plus borrowed strings); replay `measure` keys moving.

PR3. Identity and commands.
- Goal: `HudCommand` with typed parameters and app-owned enabled predicates (`player_hud/command.rs`); a control's
  id compiles to `ELEM_CONTROL_BASE + ordinal` in an open-ended range (chapters get their own at the top of `u32`,
  used in PR8); `hud.nav` holds the id; `RowPop` keyed by ordinal; `activate_player_row` dispatches on the
  command; `toggle_play` extracted from the key arm (F13) and the jump from `commit_jump` with the signed step in ns and the keep-HUD flag as parameters (`seek_jump.rs:16-18`,
  `:37-46`)
  (F14); Up Next's keys are fixed ids and `countdown_may_run(bare, row_focused, focused == play_next)`.
- Files: `appkit/src/player_hud/*`, `appkit/src/up_next.rs`, `screens/src/player/{mod,seek_jump,input}.rs`,
  `src/app/playback.rs`.
- Verify: classic's presses map to today's actions one for one (a table test over every `BTN_*`); two seek controls
  inside the burst give one target (the burst rule); a seek control press keeps the HUD up; `make check`.

PR4. Caption band as a value.
- Files: `media/src/player/text_style.rs`, `appkit/src/player_hud/*`, `screens/src/player/{mod,ass_subtitles}.rs`,
  every F28 reader, re-derived in the PR by `grep -rn 'SUB_CEIL_Y\|HUD_CONTROLS_TOP_Y\|HUD_LIFT'` (the list in the
  body).
- Verify: the tie test and `ass/tests/synth.rs:445` for `CLASSIC`, a non-classic band test, the
  `MIN_CONTROLS_TOP_Y` test (a band exactly at the bound keeps the largest block on screen through
  `caption_geometry_for`, `hud_lift` and the libass lift); `make test-hostsim C=plx_media` if a hostsim test reads it.

PR5. `appkit::hud_format`.
- Also: the text-fit and SVG lint codes (`HUD-E0601`, `HUD-W0602`, `HUD-E0701`, `HUD-E0702`), and the compile result
  keyed per (package sha256, app version, language).
- Files: `appkit/src/hud_format/{schema,parse,svg_subset,compile,lint}.rs`, `appkit/src/lib.rs`, `ui/src/rec.rs` and
  `ui/src/rec/strict_json.rs` (made a shared `pub` module, not a second duplicate-key parser), the sim's argument
  parsing (`--hud-lint`, no new `[[bin]]`, no Cargo change).
- Verify: goldens (valid and invalid fixtures, one per invariant and per command), `make test-crate C=plx_appkit
  T=hud_format` with a nonzero count, seeded mutation tests, `make check`. Risk: schema churn before PR7 proves
  classic; mitigated by reviewing PR5 with PR7 and by `builtin_classic_passes_every_invariant` here.

PR6. Schema, examples, site: `plxnative-sim --hud-schema`, `--hud-export builtin:classic`,
`site/hud/schema/1.0.json` committed as a golden with a drift test, `site/hud/examples/` linted in CI. No crate.

PR7. One renderer for the compact HUD.
- Goal: `player_hud::render` draws a `CompiledHud` from `HudModel`; `classic.json` embedded; the hand-written
  arrangement deleted when the captures match. Widgets extended in place: `TransportButton` (`bare`, `ring`),
  `Playbar` (fill, track, thickness, knob, clocks, mark, grab), `up_next::layout_of` (its `Align`), the band `chrome`
  split, `w: fill` as `title_right`'s rule, `hairline` (axis), `HUD_TITLE`. Menus anchor from
  `HudGeometry` through PR1's placer. `ClockWarm` prewarms the clock sizes the package uses (`player_hud.rs:109`,
  `:126`, test `:2400`), so the cold-clock drop does not return. A dev trigger `plxnative-hud=<path>` under
  `devtriggers` (F41) boots the sim with a package.
- Verify: pixel-identical compact, Up Next and Skip states; the reference compact state renders in the sim; the
  TV check with `ARM_PROFILE=release` and the `profile-tv` tools, which sets the compact ceiling (Cost).
- Risks: frame time on the TV (the draw list must not allocate).

PR8. Sections.
- Goal: `HudState::sections` and the two policies (D6); classic's strip and card re-expressed and the panels'
  presentation deleted; the strip on `cards::Shelf` with the three in-place extensions (D11); `ChipRow` moved to a
  shared appkit module; `rect.bleed` and `rect.h`, `actions.axis`, `text.leading`. `OverlayKind::Info` and `OverlayKind::Chapters` go: 48 sites in 9 files
  (`git grep -n "OverlayKind::Chapters\|OverlayKind::Info" -- 'rust-modules/*.rs' | wc -l` prints 48, `-l` 9);
  Tracks, More and Timing stay overlays. The fps scenes selected by overlay word (`tests/manifest.json:1418`
  "info", `:1577` "chapters") move to the HUD's section word; how the heartbeat publishes it is UNVERIFIED (today
  the word is the topmost surface's `Screen::name`) and this PR settles it.
- Verify: classic's info state pixel-identical; classic's chapters state identical except D11's stated changes
  (scroll rule, focused-name marquee), shown before and after; modal Down-to-tabs and BACK sequences pinned; the
  reference's three states render; TV calibration of the one- and two-section ceilings (Cost). Replay: no fixture
  drives the player (F33), but if a recorded state SHAPE string moves (the player screens' shapes gain the section
  set and the control ids), `SCREEN_SHAPES_PIN` moves and all three anchors are re-recorded for the shape alone
  (README `:115-127`).

PR9. The More fold.
- Goal: `more_menu::Action::Hud(HudCommand)` performed through PR3's performer; the folded rows from the compiled
  package; i18n keys in all eleven catalogs (`be` looked up per the repo's dictionary rule) with a `fit_report` over
  More's FormTable (new keys only for Previous, Next and the track-menu row; R2); `OverlayKind::More`'s
  address becomes a `MorePage` so `page: speed` opens on Speed.
- Verify: a package with no previous/next folds both and the rows perform them; classic folds nothing (its More
  root unchanged, pixel-identical); `fit_report` clean in every shipped language.

PR10. Store, local install, selection, drop folder.
- Goal: refuse the id `builtin:classic` from every source that is not compiled in (the format accepts that one
  literal so the built-in's own file parses; a sideloaded or registry file claiming it must never collide with the
  built-in's Settings entry or the fallback target). `data::hudpacks::HudStore` (pure; events `Refresh`, `Install`, `InstallDrop`, `Selected`, `Removed` in and
  the answers `Scanned`, `Read`, `Validated`, `Written`; effects `ScanDir`, `ReadFile`, `WriteFile`, `Delete`,
  `Validate`, `Selection`, `Notice`), runners in `app/adapters/huds.rs` on the storage worker, `paths::hud_candidates()`,
  a `Profile` `setting!` with the install fallback, and `SettingsPage::PlayerHud`, a FormTable keyed by package id
  (not a `PickerKind`, a static vocabulary, F32) with a `fit_report`; a compile failure in the current language falls
  back per language with the `hud: fallback` line and the Settings reason (`settings.hud.unfit_language`, eleven catalogs,
  `fit_report`), Text fit.
- Files: `data/src/hudpacks.rs`, `ci/module-layers.ini`, `docs/module-layers.md`, `ci/machines.ini`, `base/src/paths.rs`,
  `plex/src/plex/session/prefs.rs`, `screens/src/family.rs`, `screens/src/preferences*`, `src/app/adapters/`.
- Verify: a dropped or fetched file with id `builtin:classic` is refused and the built-in stays selectable; machine
  tests (event in, effect out), `ci/check-machine-shape.py`, ledgers not grown, the sim with a fixture
  pack in the drop folder, the TV under the Dev Mode install.

PR11. Install from address: the bounded fetch in `net/src/net.rs` (`fw-compat-reviewer`), the Fetch effect, the
address row. Verify with a mock HTTP server, then a real GitHub release fetch on the TV and the CA question.

PR12. Registry repository: `plxnative-huds` with CI (lint, xvfb previews, diff), the publisher (index, packs, revoked
list), Pages, and the authoring kit (`AGENTS.md`, `llms.txt`, `AUTHORING.md`, examples, the schema mirror, the
schema/parser agreement test); in this repo, the `plxnative-sim` release artifact. Verify with a seeded PR of two
sample HUDs. Risk: the pinned simulator drifting from shipped apps; pin per app release and render each entry with
the oldest app in its range. Also: the release re-lint job (`repository_dispatch` from this repo's release workflow,
plus a weekly schedule), the `unfit` and `app.max` writes and the author issue, and the simulator-to-TV skew measurement
that sets `CI_TEXT_SKEW` (Text fit; its TV part under the TV lock).

PR13. Gallery: a Settings FormTable page with preview art through the image cache, the refresh policy, updates by
`version`, revocation, `unfit` entries hidden or offered as their update (Text fit). Verify with mock index tests
(304, offline, revoked, format too new, unfit in the current language), sim captures, a TV pass.

PR14. Signed index: ed25519 over `index.json`, two accepted public keys compiled in, the key in the registry's
Actions secrets, a rotation plan; tests with good, bad and missing signatures; the build-bench table or the
fw-compat review, by the primitive chosen.

PR15. Authoring tools: `tools/hud-dev` (serves a package over LAN http with a version counter; the TV re-fetches a
dev source on a key while the dev row is focused), `plxnative-sim --hud <file>` rendering the state matrix locally,
later a static web editor on the site.

PR16. A `cargo fuzz` package over `parse + compile` and the SVG checker, with its `make build-bench` table.

PR17. Owner-gated: a per-state caption band with a lift clamp, so an open section lifts captions clear of itself;
it changes classic's info and chapters frames, which is why it waits for the owner.

No PR adds a dependency before PR14 (the schema is written from the descriptor table, F41). PR11 and PR14 may touch
the curl or crypto path and get `fw-compat-reviewer`. Each PR runs `doc-claim-auditor` after behaviour changes and
`pr-reviewer` on its combined diff before it opens.

## Verification summary

- Host: `make check` per PR; `make test-crate` loops; `make test-hostsim C=plx_media` for the caption readers;
  `make check-nodef` after Rust edits.
- Goldens: `builtin_classic_passes_every_invariant`, `classic_is_pixel_identical`,
  `reference_passes_every_invariant`, one invalid fixture per invariant, the schema drift test, the agreement test.
- Sim: the full state matrix for classic before and after PR2, PR7 and PR8; the reference's three states.
- TV: the cost calibrations in PR7 and PR8 (`ARM_PROFILE=release`, `profile-tv`); the drop folder and the address
  install under Dev Mode; HBC if such an install exists here (UNVERIFIED).

## Out of scope

- Scripting, WASM, expression languages, handlers, macros; custom fonts; raw colour literals in code; free text from a
  package; theming of other screens.
- Replacing any app-owned surface (I1). Commands that write the Plex record, change a setting, start a network
  request of their own, exit the app or delete. Direct quality and speed buttons (reached through `open_more`).
- Press-and-hold commands (FF/REW-style held controls) and a hold behaviour per control: hold stays app-owned.
- Top-anchored interactive layouts (1.1). A designable Skip pill (a later minor). Lifting captions clear of open
  sections until the owner approves PR17.
- A relay or backend that stores uploads, a LAN listener on the TV, USB until verified. Ratings, comments or payments
  in the registry; telemetry of the HUD id.

## Unverified items

- Rough caption and block arithmetic (Captions, the reference estimates); the 0.871 crop scale.
- `MIN_CONTROL_SIZE`'s value: 26 px, derived and not tried on the set: the narrowest seat the app's own screens place (a Library jump-rail letter at `RAIL_MIN_PITCH`, `screens/src/library/layout.rs:33`); the smallest control face is 56 (`ui/src/page_header.rs:56`), which would reject the reference's 52 (Package format).
- Whether `p.text`'s y is the glyph top or a baseline (changes no offset; Package format, `leading`).
- Whether the strict reader exposes an object's pairs in document order (state-keyed values).
- Whether `card_row`'s tile paint can draw the over-video rim in place of the shader ring (D11).
- The heartbeat word for an open section (PR8).
- Whether a rooted set runs under one of the two jails; HBC availability for testing; USB mounts.
- The TV's CA set and `objects.githubusercontent.com`; DNS for a registry subdomain; GitHub's unauthenticated rate
  limit figure; Homebrew 4.0's API move.
- OpenSSL EVP reachability through the TV's libcrypto.
- External format facts (JSON Schema 2020-12 support in Python `jsonschema`, DTCG, Adaptive Cards, DivKit, Lottie,
  `llms.txt`).
- How far the simulator's and the TV's text widths differ, and the firmware's SDL_ttf and FreeType versions (Text fit).
- Which step of the release workflow hosts the registry dispatch (Text fit).
- Which attributes and elements a later `art` kind's subset should admit beyond the gradients (SVG assets).
- What a held OK does on a HUD disc today: not read in this session; it stays app-owned whatever it is.
