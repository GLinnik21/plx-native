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

`ui/widgets.rs::search_pill()` is `tab_count() - 1` — Search is always the LAST pill, and since
2026-09-05 that is the only thing about its position that holds still. The row is
`Home`, then one pill per type that has a FAVOURITE library, then Search: two to four stops, because
`browse::tab_count()` now counts `tab_kinds()`, and a type whose last favourite is switched off
draws no pill. Movies and TV Shows are still type destinations whose concrete owned/shared section
is resolved later.

**So an INDEX is no longer a stable name for a destination** — this paragraph used to claim exactly
that, and the claim was the load-bearing half. Store a `Pill` (`Pill::Section` carries a
`browse::SecKind`) and resolve it on READ through `widgets::pill_of`, which answers `None` for a
type that has no pill today. Every positional cursor in the app was converted for this: Search's own
`STRIP`, `library::TAB_P`, Home's `HERO_PILL`, and `Nav::Home`'s pending focus.

It is also the one pill that is a **mark instead of a word**, so it is square (60×60) and skips the
label padding, inked through the same `TabPill::mixed_ink` the labels use so it travels under the
focus capsule with them instead of being a separate colour story.

### The layout is dictated by the keyboard

The TV's own panel covers the bottom **324px**, MEASURED off four device captures
(`screens/search/layout.rs`'s `KEYBOARD_H`; it was a 380px guess until 2026-08-15). The rule the
numbers come from: **with the keyboard raised, nothing the app owns hides behind it.** The field,
the first shelf's heading and that shelf's full row of posters are sized to land above its top edge
— `CONTENT_TOP` 300 + `HEAD_TO_ROW` 60 + a 375-tall poster = 735, against a panel edge at 756.

Both of those numbers have moved since this section was first written, and in opposite directions:
the keyboard turned out to be 56px shorter than the guess, and `CONTENT_TOP` moved down twice — once
with the top bar, and once when the field stopped being an 820×60 capsule and became a full-width
`size::HERO` line with its scope block underneath. What did not move is the rule, and
`layout.rs`'s `the_first_shelfs_whole_row_clears_the_raised_keyboard` is where the arithmetic is
graded rather than described.

That is also why **nothing scrolls while the panel is up**: the result set has to be stable under
the user's eyes while they are still typing.

`search::recents::CAP` is **four, not five**, for the same reason — with the keyboard raised the header, the
rows and the Clear control all have to finish above its edge. The fifth is *dropped*, not scrolled:
a list you cannot see the end of asks to be paged, and there is no paging in this product.

### The caret and the one-character hint stay in the field

`screens/search/render.rs` states it: the caret is 5px wide, one HERO cap band tall and **blinks** in
530ms phases while the television's keyboard is up. That is intentional device feedback: the
solid bar specified by the design component read on the couch as a field that was not accepting
input. The implementation still obeys `ui::idle` — the whole-frame present gate cannot see a clock
structurally, so `step_blink` calls `invalidate()` only when the phase flips. The open keyboard
therefore costs about two presents a second instead of holding the GL loop awake, and with the
keyboard down the phase parks ON and costs nothing.

**A blank field's caret sits at the field's own START**, before the placeholder, not after it — the
placeholder is chrome nobody is editing, so it supplies no insertion point of its own to trail
(owner-reported 2026-09-03; `field.rs`'s blank branch feeds `run_layout` a `caret_w` of zero rather
than the placeholder's own measured width).

### Focus is ink only — two bright endpoints, never a fill

For one day (2026-09-03, issue 22) the field had THREE visual states, not two: idle and editing
drew no plate, but **focused-but-not-yet-editing** lit up with a flat `theme::ACCENT`/
`theme::ACCENT_INK` fill — the same pair every other focused control in this app fills with —
because ink alone read too close to idle from the couch (owner feedback). It shipped, and the
owner rejected it on sight the next day: "you made the search bar background white, while I wanted
text to be white". **The plate is gone (2026-09-04)** — `field.rs`'s test suite carries a narrow
regression tripwire, reading the file's own source for the exact `p.rect(field_box`/`theme::ACCENT,`
shape issue 22 shipped (the same technique `diag::scrub` uses for its own absence property), so
that specific reintroduction fails loudly. It is a tripwire for the bug that already happened, not a
proof that no fill can ever reappear under a different name or shape — the actual contract is the
plain-English one above, upheld by review and device/simulator verification each time this control
changes.

Focus is carried by **ink alone**, permanently — the design system's `SearchField` contract, which
forbids a rim, rule or capsule OUTLINE and was never actually license for a fill either. What
answers the couch-legibility problem the plate was trying to solve is two DIFFERENT bright ink
endpoints instead of one, picked by `editing` alone (`field::draw`'s `ink_target`, never a second
spring, riding the field's existing `hot` spring either way): **focused, not yet editing** — no
caret, no keyboard, nothing else on screen to say "the remote is on this" — crosses to
`theme::FIELD_WAITING_INK`, a genuinely pure white; **editing** drops back one stop to
`theme::FIELD_EDITING_INK` (`TEXT_PRIMARY`, and the caret's own colour), because the blinking bar and
the open keyboard already carry that state's own signal. Both ride the same wide
`TEXT_SECONDARY`→target step that replaced the original one-stop `TEXT_HEADING` fade (owner
feedback, 2026-09-02).

One character short of `MIN_QUERY`, `one more character` appears at BODY after that caret. It is
part of the field, not a second caption above the scope line: it explains the control at the place
the eye is already following and is consumed by the next keystroke. The scope remains one CAPTION
line at `SCOPE_Y` in every query state.

The practical consequence for anyone adding motion here: **a clock-driven animation must report
only when its pixels change.** See `fps:search-idle` in §5, which is the assertion that catches an
animation keeping a settled screen awake.

### The state machine, the geometry and the drawing are three files, not one

`screens/search/mod.rs` (`SearchScreen`) owns the zones, focus, scroll and the draw ORDER and
nothing else; `layout.rs` is the pure geometry both `mod.rs`'s placement and `render.rs`'s paint
read from (`FIELD`, `CONTENT_TOP`, `KEYBOARD_H`, the field/results/recents rects); `render.rs`
paints from the instance and its retained frame views only — no live Search/roster/focus reads —
so a region can never draw a different focus than the state machine believes in. Two more files
round it out: `draft.rs` is the instance's own unacknowledged editing state (the store owns the
committed query/results; the draft owns edits until the next store notice acknowledges them), and
`memory.rs` is the entry-owned `PageMemory` a return visit restores from (current focus itself
stays in the container's `ReturnState`).

Recents are rows and not a shelf (nothing there has artwork), they are the **user's own words** and
stay editable in place, and Clear leaves the list to become a Button — a verb never sits in the same
column as the words you searched for. They persist in the session file beside the roster behind a
soft-failing deserializer, so a corrupt entry costs that entry and never the session.

The recents data owner is now `search/recents.rs`: profile-generation cache, immutable term
publications, normalization, and worker persistence. `SearchCmd` carries remember/clear requests
with the captured profile generation; the store refuses a command after that generation changes.
Pending saves coalesce per profile, so switching profiles cannot replace another profile's
accepted history. Returning before the worker drains reads that pending history. The worker
checks the captured installation and account credentials against the session under its IO lock;
an old account's pending history cannot be written into a replacement account. The queue admits
64 distinct profiles at once (a resource ceiling, not a Plex limit); when full it refuses a new
profile's edit without evicting accepted saves and retries the background drain.
The renderer in `screens/search/render.rs` keeps only geometry and a glyph cache keyed on that
publication. `SearchSnapshot` includes recents beside query/results and is retained at the bridge's
frame boundary. `SearchScreen` consumes these retained views and never reads the session or stores
directly during draw.

Text editing itself uses `ui/text_buffer.rs`: an owned UTF-8 buffer and caret, with literal
insertion, character-wise movement/deletion, and the TV's whole-commit prediction rule. The owned
screen's `draft.rs` retains a draft across successive input events while the frame's store view is
frozen. A prediction publishes the completed replacement once, without exposing an intermediate
deleted word to the store.

The normalized input vocabulary carries a whole immutable `TextEdit::Commit`, plus Backspace,
Clear, Left and Right. Keyboard ownership edges take effect in delivery order, including several
edges in one frame; the page keeps its focus read while `Cx.owner` is `System(Keyboard)`.
An ownership change cancels both an active page gesture and its queued hold/commit result.
Pending inputs include their payload in the canonical state hash. The fixture recorder/replayer
round-trips these events. Screen keyboard requests are distinct from OS observations: Input
binds accepted requests to the requesting instance, rejects stale opens/closes, then invokes the
main-thread adapter's existing `textinput::start/stop` operations. Cover and suspend release that
binding. Both the binding and queued keyboard requests are included in the state hash.
The owned path normalizes SDL and FIFO commits at ingress, before later edit keys, and uses
`adopt` (never `start`) for a panel observation. Text recording preserves commit boundaries,
original timing/source, and the observed panel capability, and replay reuses that observation
rather than probing its current platform.

**Live cutover is finished.** `Route::Search` mounts `screens::search::SearchScreen`
unconditionally — there is no `AppMounter::search_owned` flag any more, no legacy ingress/lifecycle
ladder in `app/{nav,input,run}.rs` for it to coexist with, and no separate "contract reference"
route: `screens/search` is both the production implementation and the thing its own tests grade.
It consumes `AppViews.search`, keeps an unacknowledged draft, uses engine-owned focus groups and
supplies query/profile-guarded entry restoration data. Only Search-store notices acknowledge a
pending draft; an unrelated store notice cannot treat matching frozen text as confirmation of an
edit that has not reached the next frame publication. Its renderer reads retained query, result
and source-scope publications. Keyboard hooks and navigation/menu request execution run
unconditionally. Action resolution checks the retained selection's server and local identity; BACK
returns to Home without selecting its pill, unlike activating the Home pill itself. The menu
opener uses the owned page's captured focus and drawn geometry. The legacy `ui/search/` module
tree (`mod.rs`, `field.rs`, `results.rs`, `empty.rs`, `recents.rs`) is deleted; its 83 test bodies
were reconciled against the owned screen, store and shared-component contracts before deletion —
68 Ported under the same or a stated name, 13 Covered by an existing owned test, 2 Retired as
implementation-history assertions with no migration contract of their own
(`docs/measurements/search-render-contract-ledger.md` is the row-by-row record). Host tests are
still not visual verification — device-level pixel and text-rasterization verification of the
owned screen is a separate obligation this cutover does not retire (§5's regression scenes are
host-side rate/frame-time gates, not that proof).

The owned page requests discovery and Search debounce/landing work once per active Tick through
the store dispatcher. Its external Cover/Suspend/WillLeave/Unmount events release editing without
filing the draft in recents. A background keyboard observation is ordered after earlier input and
also releases the native start latch, so a later OK can reopen the panel.
Unlike the legacy wheel-as-D-pad path, the owned path follows specification §7.3: a wheel scrolls
the document without changing focus. Focus movement reveals the selected region again; editing
parks the document at the top. Page hit clips stop below the standing tab strip even though the
document continues painting underneath its glass.

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
  `<query>`. Read once at boot in `app/boot.rs`, through `dev::read` like every other trigger; it
  seeds `stores::search::SearchCmd::SetQuery` directly rather than driving a screen method.
- **`/tmp/plxnative-searchosc`** — sweep the result shelves' focus down↔up perpetually: one step per
  350 ms, reversing every 3 s, the same cadence `homeosc` and `libosc` use so all three read the same
  in a log. It injects synthetic D-pad input through the real dispatcher (`bridge::script_key`),
  exactly as `homeosc` does, rather than reaching into the screen's focus state directly.

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
