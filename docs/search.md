# Search

The Search screen: what it is, the decisions behind it, and — the part that would otherwise be
lost — the **research into the television's on-screen keyboard**, which is where the expensive days
went. If you read one section of this file before touching text entry, make it §3.

**Scope: the user's own servers, and nothing else.** Every result comes from a PMS the account can
reach. Plex Discover / Watchlist / the "Movies & Shows on Plex" catalog are deliberately absent —
see §6.

---

## 1. The screen

### It is a peer, not a page

`Route::Search` sits beside `Home` and `Library`, not on top of them (`rust-modules/src/app.rs`,
the `Route` enum's `Search` arm and its doc comment). It is reached from the strip's last pill and
BACK from it returns to Home, so it needs no `ui::trail` node — **what Search opens stacks; Search
itself does not.** It wears the shared top tab bar (`route_wears_tab_bar`), which is what makes the
Home↔Search transition a cross-fade of the *page* with the chrome held still, exactly like
Home↔Library.

Two of the route's match sites are exhaustive and would have failed the build if an arm were
missing. Thirteen are `_` catch-alls that would not, and three of those are worth knowing about
because their default is silently wrong rather than loud: the draw dispatch ends
`} else { home_draw() }`, the BACK arm ends `running = false`, and the heartbeat's route name ends
`_ => "home"`. That last one is why the FPS scenes in §5 can key on `route=search` at all — without
its arm every one of them would have graded Home and passed.

### The pill goes LAST

`ui/widgets.rs::search_pill()` is `tab_count() - 1`. Appending rather than prepending is the entire
reason the pill cost nothing: `Home = 0, sections = 1..=n` stays true as written, so no
`pill - 1 → section` conversion moved, and the only new rule is `is_search_pill` — asked in one
place. Prepending would have shifted every conversion to `+2` at eight sites.

It is also the one pill that is a **mark instead of a word**, so it is square (60×60) and skips the
label padding, inked through the same `TabPill::mixed_ink` the labels use so it travels under the
focus capsule with them instead of being a separate colour story.

### The layout is dictated by the keyboard

The TV's own panel covers the bottom ~380px (`ui/search/mod.rs`'s `KEYBOARD_H`). The rule the
numbers come from: **with the keyboard raised, nothing the app owns hides behind it.** The field
(`FIELD`, 820×60 at the app's side margin), the first shelf's heading and that shelf's full row of
posters are sized to land exactly on its top edge — `CONTENT_TOP` 248 + `HEAD_TO_ROW` 60 + a
375-tall poster = 699, against a 1080-tall authored surface with 380 taken.

That is also why **nothing scrolls while the panel is up**: the result set has to be stable under
the user's eyes while they are still typing.

`MAX_RECENTS` is **four, not five**, for the same reason — with the keyboard raised the header, the
rows and the Clear control all have to finish above its edge. The fifth is *dropped*, not scrolled:
a list you cannot see the end of asks to be paged, and there is no paging in this product.

### The caret does not blink, and that is a system decision

`ui/search/field.rs` states it: the caret is 3px × 34px and **solid**. A blinking caret is a
clock-driven animation, and `ui::idle` — the whole-frame present gate — cannot see one structurally;
it detects motion from the spring integrators and from explicit `invalidate()` calls. A blink would
therefore either freeze (drawn once, never repainted) or hold the GL loop awake forever and give
back the measured idle saving. Both failure modes have shipped in this app before, in
`Xfade::tick` and `Spinner::draw`.

The practical consequence for anyone adding motion here: **a clock-driven animation must report, or
it must not exist.** See `fps:search-idle` in §5, which is the assertion that catches the second
failure mode.

### Five files, because the state machine is not the drawing

`ui/search/mod.rs` owns zones, focus, scroll and the draw ORDER and nothing else; `field.rs`,
`recents.rs`, `results.rs` and `empty.rs` each draw one region from the same per-frame `View`
snapshot, so a region can never read a different focus than the state machine believes in.

Recents are rows and not a shelf (nothing there has artwork), they are the **user's own words** and
stay editable in place, and Clear leaves the list to become a Button — a verb never sits in the same
column as the words you searched for. They persist in the session file beside the roster behind a
soft-failing deserializer, so a corrupt entry costs that entry and never the session.

---

## 2. The data layer

`GET /hubs/search?query=…&limit=…` — `plex::Client::search` in `rust-modules/src/plex/hubs.rs`,
written long before this screen and dead until now. Four facts were **measured against PMS 1.43.3**
rather than taken from the spec, and each one decides something:

| measured | consequence |
|---|---|
| A one-character query returns every hub empty | `search::MIN_QUERY` is 2 — the first keystroke of every search costs no round trip |
| Hub ORDER moves per query (`sta` ranks people first, `star` ranks films first) | the shelf order here is FIXED (`search::KINDS`) and ranking is honoured only *inside* a shelf; reordering rows per keystroke would move the row under a typing user's focus |
| Items arrive in **two** containers — `Metadata[]` for `movie`/`show`/`episode`, `Directory[]` for `actor`/`director`/`collection` | `search::Item` has two variants (`Media`/`Tag`) instead of being one struct. `plex-openapi.json`'s own worked example disagrees with the server, which is why this was probed live; see the table in `Hub::directory`'s doc in `plex/models.rs` |
| A search response carries **every** hub type the server knows, most with `size: 0` | `Hub::size` is the field that says which shelves are worth drawing |

The `actor` and `director` hubs are merged into one **Cast & Crew** shelf. Merging in the data layer
rather than in the UI keeps "what is a shelf" a data question.

`State` distinguishes `Ready`-with-nothing from `Failed`, a lesson `browse.rs` learned the hard way:
an empty result set is an ANSWER and reads as "No results"; a fault is a fault and reads as one. An
empty store alone cannot tell them apart, and dressing an answer as an error tells the user something
untrue about their library.

**Multi-source.** `/hubs/search` answers for the one machine you asked; nothing aggregates
server-side (`docs/shared-servers.md`). So the store fans out one query per `plex::server_ids` and
merges into the shelves, which is why every `Item` carries its own `ServerId` and why a shelf heading
can never claim an owner — the owner annotation follows FOCUS and rides the focused tile's caption,
exactly as Continue Watching's does on Home.

---

## 3. Text entry — the television's own keyboard

This is the section that exists so nobody re-derives it.

### It is plain SDL, not a webOS call

Stock `SDL_StartTextInput()` / `SDL_StopTextInput()` raise and dismiss the TV's system keyboard.

`SDL_webOS.h` **misleads by omission**: it declares eight entry points (cursor visibility, panel
resolution, refresh rate, and the five exported-window calls) and not one of them is a keyboard. The
backend is not in the webOS extension API at all — it is inside **LG's Wayland video driver**:
`Wayland_CreateDevice` writes four real hooks into `SDL_VideoDevice`
(`WebOSHasScreenKeyboardSupport`, `Show`, `Hide`, `IsShown`), `SDL_StartTextInput` dispatches to the
second, and `WebOSShowScreenKeyboard` is a complete `text_model` IME client. Typed text comes back
as an ordinary **`SDL_TEXTINPUT`** event, so `app.rs`'s existing `SDL_PollEvent` loop already sees
it. moonlight-tv 1.5.8 shipped exactly this against the TV's own SDL.

**Linking is the plain `extern "C"` case, not `dynlib!`.** Verified with
`tools/fwcompat.py --lib libSDL2-2.0.so.0 --grep TextInput`: all **14** firmware images in the
inventories — 1.2.0, 1.4.0, 2.2.3, 3.4.0, 3.9.2, 4.4.2, 4.10.0, 5.3.1, 6.4.0, 7.4.0, 8.3.0, 9.2.0,
10.2.0, 11.2.0 — export `SDL_StartTextInput`, `SDL_StopTextInput`, `SDL_SetTextInputRect` and
`SDL_IsTextInputActive`, and `--grep ScreenKeyboard` finds `SDL_HasScreenKeyboardSupport` /
`SDL_IsScreenKeyboardShown` on the same 14. The SONAME does not move either, so this is not a
`dynlib!` candidate — that module is for libraries whose *version* varies, and moving one there
trades link-time symbol checking for tolerance nothing here needs.

That symbol table says nothing about whether the **panel actually rises**, though: those are stock
public API in every SDL2 build ever made. `textinput::available()` therefore probes at runtime rather
than assuming.

### Three traps

**1. The event is SHIFTED, and the vendored header lies about it.**

webOS inserts a `Uint32 inputSource` before the text, so the UTF-8 bytes start at **+16**, not +12.
Both halves are checkable on the dev Mac without a television:

- `include/SDL2/SDL_events.h` — the tree we compile against — declares
  `SDL_TextInputEvent { type, timestamp, windowID, text[] }`, i.e. text at **+12**. That tree is
  stock **2.0.4** (`include/SDL2/SDL_version.h`).
- The NDK sysroot's fork copy
  (`$(WEBOS_SDK)/arm-webos-linux-gnueabi/sysroot/usr/include/SDL2/SDL_events.h`, SDL **2.24.1**)
  declares `{ type, timestamp, windowID, inputSource /* webOS specific field */, text[] }` — text at
  **+16**.

This is the same class of bug as the `SDL_KeyboardEvent` shift `app.rs` already reads around with
raw offsets, and it has the same fix: read the bytes, do not trust the struct. Note the offset is a
`cfg`, not a constant — under `hostsim` desktop SDL2 is stock and the offset really is +12, so a
single hard-coded number ships garbage on one of the two platforms.

**2. `SDL_WINDOW_INPUT_FOCUS` is a silent precondition.**

`SDL_StartTextInput` looks for a window carrying `0x200` (`include/SDL2/SDL_video.h`) before
dispatching to the driver hook. Our window is created with
`SDL_WINDOW_OPENGL | SDL_WINDOW_FULLSCREEN` and nothing else — `app.rs`'s `SDL_WINDOW_FLAGS` is
`0x2 | 0x1` on the device — so it carries neither `SHOWN` nor a focus flag at creation. **If the flag
is clear the panel never rises, silently and with no error return.** That is why the boot probe logs
the flag rather than trusting it: a keyboard that does not appear and a keyboard that appeared and
was dismissed look identical from inside the app.

**3. A reopen wedge we inherit and cannot patch.**

The panel cannot be reopened after dismissal (moonlight-tv issue #435, reproduced on webOS 7.4). The
community fix lives in webosbrew's *bundled* SDL fork; we call the **television's own** SDL, so we
get the bug with no patch. `SDL_SetTextInputRect` is a no-op here too — open and close, no
positioning.

This is a real constraint on the interaction design, not a footnote: the screen must not treat
"dismiss and raise again" as a normal gesture, because on some firmwares it is a one-way door.

### On the simulator, which is a different keyboard stack entirely

Two host-side findings, recorded because both cost time today and neither is a statement about the
television.

**The boot probe's answer on macOS is `keyboard: support=0 active=1 focus=0 winflags=0x26`.** That
is **trap 2 observed**: `0x26` is `OPENGL | SHOWN | RESIZABLE` (`app.rs`'s `hostsim` arm asks for
`0x2 | 0x20`; SDL adds `SHOWN`), and `0x200` — `SDL_WINDOW_INPUT_FOCUS` — is not in it. The trap is
therefore reproducible without a television, which is the useful part; what it does **not** tell you
is anything about the TV's own answer, since `support=0` here just means desktop SDL has no screen
keyboard at all.

**Do not push a synthetic `SDL_TEXTINPUT` through `SDL_PushEvent` on the host — it SIGSEGVs inside
SDL.** macOS `libSDL2` is **sdl2-compat**, a shim forwarding into SDL3, and SDL3's text event
carries a `char *text` **pointer** where SDL2 carries an inline `char[32]`; the compat layer
dereferences what it is handed. There is no Rust panic and no log line — the process is simply gone.
The existing remote-FIFO injection is safe by luck of shape: key events and `ck:` pointer clicks are
all scalar fields, and nothing in them is read as a pointer. Anything that wants to test the field
headlessly must go in **above** SDL — through `textinput`'s own buffer — not by forging the event.

### Two dead ends, so nobody spends a day on them again

- **The Luna route.** There are only four `com.webos.service.ime/*` methods, and all of them sit in
  ACGs this app does not hold: `pkg/appinfo.json` declares no `requiredPermissions` at all (and
  `docs/distribution.md` records that this is correct — neither Kodi nor Moonlight declares one), so
  the app is granted `["public"]`. Declaring more is not a fix; those groups are not grantable to a
  homebrew app.
- **A physical USB or Bluetooth keyboard.** `/dev/input` is not mounted into our jail.
  `rust-modules/src/remote.rs`'s module doc records the general case and why it generalises: on this
  build the wayland compositor (`surface-manager`) opens a **fixed** set of evdev nodes at boot and
  never picks up hotplugged or `uinput` devices, and LG's
  `com.webos.service.tv.keymanager/createKeyEvent` injects into the webOS *web-app* key layer, not
  the wayland path we read. External input does not reach our surface by any route tried.

### Provenance

Re-verified on the host while writing this file: the two SDL header trees and their versions, the
window flags, `SDL_WINDOW_INPUT_FOCUS`'s value, `SDL_webOS.h`'s eight entry points, the 14-firmware
symbol sweep, and `appinfo.json`'s absent `requiredPermissions`. Measured on the **simulator**, and
so about a Mac and not a television: the boot probe read-out and the `SDL_PushEvent` crash above.
Recorded from the device and
disassembly work that produced `rust-modules/src/textinput.rs`'s module doc, and **not**
re-established here: the four `Wayland_CreateDevice` hooks, `WebOSShowScreenKeyboard`'s `text_model`
client, the moonlight-tv reproduction, the four Luna IME methods, and the jail's missing
`/dev/input`.

---

## 4. Driving it headlessly

Neither the TV harness nor the desktop simulator can type, so both would otherwise only ever see the
empty state.

- **`/tmp/plxnative-search[=<query>]`** — boot straight into Search with the field already holding
  `<query>`. Read once at boot in `app.rs`, through `dev::read` like every other trigger.
- **`/tmp/plxnative-searchosc`** — sweep the result shelves' focus down↔up perpetually: one step per
  350 ms, reversing every 3 s, the same cadence `homeosc` and `libosc` use so all three read the same
  in a log. It drives the real `ui::search::move_focus`, not an imitation of it.

**`searchosc` does not reach the screen on its own** — pair it with `plxnative-search`. Neither is on
`dev.rs`'s `DIAG` exemption list, and neither should be: DIAG is for files that are pure diagnostics
(the four logs, the profiler, the remote FIFO, the capture listener, the idle-gate override), and an
oscillator is automation — it changes what the app does. Both therefore mark the boot automated and
suppress the who's-watching picker, which for these scenes is the point: a run that landed on the
picker would grade the wrong screen.

Both are behind the `devtriggers` cargo feature like every other `/tmp` read, so a `RELEASE=1` binary
compiles them out entirely.

---

## 5. The regression scenes

Two, in `tests/manifest.json` → `fps_scenes`, both keyed on `"route": "search"`. **They are a pair
and only mean something together** — the same screen with and without its oscillator.

| scene | asserts | triggers |
|---|---|---|
| `fps:search-type` | `fps_floor` — the screen still ANIMATES under a travelling focus | `plxnative-search`, `plxnative-searchosc` |
| `fps:search-idle` | `fps_ceiling` — a settled result set STOPS presenting | `plxnative-search` |

Picking the wrong assertion is how a frozen animation ships, so, restated: `loop_floor` grades
`loop=`, which counts **loop iterations** and reads ~60 with every present skipped — it proves the
app is alive and cannot see a stopped animation at all. `fps_floor` grades `fps=`, frames actually
swapped, and is the only thing that proves motion. `fps_ceiling` grades `fps=` from the other side
and is the only guard on over-reporting, which silently gives back the whole idle saving while every
floor in the suite still passes.

Two honesty notes carried in the manifest itself and repeated here because they are easy to lose:

- **`search-type`'s `fps_floor` is not a device measurement.** Every other floor in that file quotes
  a measured median and a date; this one was written while the screen was still being built and is
  chosen only to separate a frozen animator (~0.5/s — `ui::idle`'s 2 s keepalive alone) from a
  running one. Raise it to a real median the first time it runs green on a television.
- **The query is a literal, not a symbolic key.** `run.py` resolves `item` keys against the
  gitignored overlay; it has no notion of a query, so the manifest carries the text. If a library
  matches nothing for it there are no shelves, the sweep has nothing to travel, and `search-type`
  degrades to grading the tab strip's focus springs. **Change the literal, never the floor.**

Neither scene can be graded anywhere but on the television: there is no host runtime, and `run.py`
refuses outright to grade a log carrying the simulator's `sim=1` tag against device-calibrated gates.

---

## 6. Out of scope: Discover, Watchlist, and the catalog

The official client's search returns Plex's own catalog and Discover results alongside server ones.
**Ours does not, by decision.** Those live on `discover.provider.plex.tv` /
`metadata.provider.plex.tv`, which need DNS and TLS and therefore `net.rs` (libcurl) rather than
`stream.rs`; they are an *adjacent catalog*, not the user's library, and `docs/parity-gaps.md` has
tracked them as their own gap from the start. Adding a catalog row to this screen is a separate
feature with its own client, its own store and its own failure modes — not a wider `limit=` on
`/hubs/search`.

What this screen closes, and what it does not, is written up in `docs/parity-gaps.md` under the
search entries.
