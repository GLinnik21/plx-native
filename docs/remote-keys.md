# The remote key map

**Every key this app binds, what field it is matched in, and what it does** — plus the questions
about keys that only a television can answer, written down as recipes rather than guesses.

This file is what LG's App Self Checklist rows **26** (general / IR remote), **36** (HOME),
**39** (LIVE), **40** (unsupported keys) and **45** (playback control keys) cite;
`docs/lg-self-checklist.md` is the status of those rows, this is the evidence under them.

Status taken 2026-08-26. **Everything below is offline work — read out of the harvested `libSDL2`
or quoted from a dated earlier session — WITH ONE EXCEPTION: §9's colour-button row was measured on
the dev set on 2026-08-26**, by pressing all four buttons with the log open. That exception matters
in both directions: it is the only device-fresh table here, and it is also the row that proves the
offline method can be *wrong* rather than merely incomplete (the translation table's one answer for
a colour key, `KEY_GREEN`→504, is a code the real remote never sends). What *was*
done offline is read out of the television's own harvested `libSDL2`, which is stated where used.

---

## 1. How a press reaches the app

An `SDL_KEYDOWN` / `SDL_KEYUP` from LG's SDL fork is read by raw byte offset, because the fork's
`SDL_KeyboardEvent` is **shifted four bytes** against the headers (`app.rs::decode_key`, and the
gotcha in `docs/agent-reference.md`):

| offset | field | the app calls it |
| --- | --- | --- |
| +16 | `state` | low byte pressed(1)/released(0), bit `0x100` auto-repeat |
| +20 | keysym's first word | **`wcode`** |
| +24 | keysym's second word | **`sym`** |

`sym` is an ordinary **SDL keycode**: ASCII for printable keys (`SDLK_RETURN` = 13), otherwise the
scancode with `1<<30` set (`SDLK_LEFT` = `80 | (1<<30)`).

### `wcode` is the SDL SCANCODE, not a separate webOS keycode namespace

The name is historical and it misleads. Every pair ever measured off the dev set has `wcode` equal
to the **SDL scancode** of the key whose keycode is in `sym`:

| key | measured `wcode` | SDL scancode | when |
| --- | --- | --- | --- |
| OK | 40 | `SDL_SCANCODE_RETURN` | 2026-08-22 |
| D-pad L / R / D / U | 80 / 79 / 81 / 82 | `SDL_SCANCODE_LEFT` / `RIGHT` / `DOWN` / `UP` | 2026-08-22 |
| panel delete | 42 | `SDL_SCANCODE_BACKSPACE` | 2026-08-15 |
| panel **Clear all** | 156 | `SDL_SCANCODE_CLEAR` | 2026-08-15 |
| panel `◀` / `▶` | 80 / 79 | `SDL_SCANCODE_LEFT` / `RIGHT` | 2026-08-15 |

So the fork inserts one word ahead of the keysym and leaves an ordinary
`SDL_Keysym { scancode, sym }` at +20/+24. That one sentence explains the rest of this file:

* **Codes with no `sym` are LG's own scancodes.** SDL's enumeration ends at 286
  (`SDL_SCANCODE_AUDIOFASTFORWARD`); everything from **300 upwards** here — 300/301 channel, 450
  play, 451/452 FF/RW, 482 BACK, 505 EXIT — is LG's private extension, for which SDL's keymap has
  no keycode, so `sym` arrives as 0. That is why the transport arms are matched in `wcode`.
* **A code below ~290 in `wcode` is a KEYBOARD key**, and reading one as a remote button is the
  single bug class this map has now hit four times (§4).

### The television's own translation table, readable offline

LG's fork carries the evdev-to-scancode table at **file offset `0x92840`** of
`libSDL2-2.0.so.0.4.1`: 624 `u32` entries, index = the `linux/input.h` evdev code, value = the
scancode delivered in `wcode`. It is the authority for "can this set produce that code at all",
it needs no television, and the `Provenance` column below is read out of it. With a harvested copy
of the library (the `decompile-tv-lib` skill), unpack it with `struct.unpack_from("<624I", data,
0x92840)` and print `index -> value` for every non-zero entry.

The table is otherwise byte-for-byte SDL's own standard `linux_scancode_table`, which is what makes
"scancode 33 is `SDL_SCANCODE_4`" a fact about this firmware rather than an analogy.

---

## 2. The map

`classify` (`ui/consts.rs`) resolves one `(sym, wcode)` pair to one `Key`; the ladder in `app.rs`
dispatches on that. **Two sets are bound OUTSIDE the classifier** and are listed with the rest —
being `Key::Other` does not mean unbound (§3).

### Navigation and confirm

| Code | Field | `Key` | What it does | Provenance |
| --- | --- | --- | --- | --- |
| `82 \| 1<<30` | `sym` | `Up` | four-way nav; in the player, raise/hide the HUD | evdev 103 `KEY_UP` |
| `81 \| 1<<30` | `sym` | `Down` | four-way nav; in the player, the HUD | evdev 108 `KEY_DOWN` |
| `80 \| 1<<30` | `sym` | `Left { alt: false }` | four-way nav; in the player, scrub back | evdev 105 `KEY_LEFT` |
| `79 \| 1<<30` | `sym` | `Right { alt: false }` | four-way nav; in the player, scrub forward | evdev 106 `KEY_RIGHT` |
| `13` | `sym` | `Ok` | activate the focused control | evdev 28 `KEY_ENTER` (scancode 40) |
| `88 \| 1<<30` | `sym` | `Ok` | keypad ENTER | evdev 96 `KEY_KPENTER` |
| `77 \| 1<<30` | `sym` | `Ok` | named `SDLK_SELECT` — **it is `SDL_SCANCODE_END`, see §9** | evdev 107 `KEY_END` |
| `27` | `sym` | `Back` | ESC, for dev keyboards | evdev 1 `KEY_ESC` |
| `113` (`'q'`) | `sym` | `Back` | dev keyboards | — |
| `482` | `wcode` | `Back` | **the remote's BACK** | evdev 303 |
| `461` | `wcode` | `Back` | a second BACK, "for other remotes" | **not producible on this firmware** — no table entry yields 461. Defensive and unevidenced. |

BACK reaches the app at all only because `src/main.c` sets `SDL_WEBOS_ACCESS_POLICY_KEYS_BACK`
before `SDL_Init` (§5).

### Transport (checklist #45, bound 2026-08-22)

| Code | Field | `Key` | What it does | Provenance |
| --- | --- | --- | --- | --- |
| `450` | `wcode` only | `Play` | resume, or start playback off a card | evdev 207 `KEY_PLAYCD` |
| `72` | `wcode` only | `Pause` | pause | evdev 119 `KEY_PAUSE` |
| `261` | either | `PlayPause` | toggles — its own variant, since `key_play`/`key_pause` are each one direction | evdev 164 `KEY_PLAYPAUSE` |
| `451` | either | `Right { alt: true }` | fast-forward: scrubs in the player, does nothing elsewhere | evdev 208 `KEY_FASTFORWARD` |
| `452` | either | `Left { alt: true }` | rewind, likewise | evdev 168 `KEY_REWIND` |
| `120` | `wcode` only | `Stop` | player only: stop and leave | evdev 128 `KEY_STOP` |
| `260` | `wcode` only | `Stop` | player only: stop and leave | evdev 166 `KEY_STOPCD` |
| `505` | either | `Exit` | **terminates the app** (checklist #38); no confirmation, unlike root BACK | evdev 174 `KEY_EXIT` |
| `19` | either | `Play` | legacy alternate — **this is `SDL_SCANCODE_P`, see §4.2** | evdev 25 `KEY_P` |
| `402` | either | `Play` | legacy alternate | evdev 472, unnamed in `linux/input.h` |
| `415` | either | `Pause` | legacy alternate | evdev 541, unnamed |
| `413` | either | `Stop` | legacy alternate | evdev 534, unnamed |

### Bound outside `classify`

| Code | Field | Where | What it does |
| --- | --- | --- | --- |
| `75 \| 1<<30` | `sym` | `page_dir`, Library route only | page the grid up |
| `78 \| 1<<30` | `sym` | `page_dir`, Library route only | page the grid down |
| `300` | `wcode` | `page_dir`, Library route only | CH▲ pages up (evdev 402 `KEY_CHANNELUP`) |
| `301` | `wcode` | `page_dir`, Library route only | CH▼ pages down (evdev 403 `KEY_CHANNELDOWN`) |
| `8` | `sym` | `ui::search::key`, while editing | backspace |
| `156 \| 1<<30` | `sym` | `ui::search::key`, while editing | the panel's **Clear all**. **Not producible from the evdev table** — it reaches us through the fork's own text/IME path, which is why it is measured but has no evdev provenance. |
| `48..=57` | `sym` | `ui::profiles`, PIN keypad | type that digit into the PIN. `profiles::digit_of` also reads the range out of `wcode`, where it is **not** a digit — §9. |

### Swallowed

| Code | Field | `Key` | What it does |
| --- | --- | --- | --- |
| `484` (`0x1e4`) | `wcode` | `PointerHidden` | the Magic Remote reporting its pointer auto-hid. The ladder's arm has an empty body — the press is consumed there rather than reaching the arms below. evdev 614, an LG-private code and not a key. |

### Reachable but deliberately unbound

| Code | What it is | Why |
| --- | --- | --- |
| `269` | `SDL_SCANCODE_AC_HOME`, evdev 172 `KEY_HOMEPAGE` | §5 |
| `270` | `SDL_SCANCODE_AC_BACK`, evdev 158 `KEY_BACK` | not the remote's BACK, which is 482. Never observed on this set; adding it needs a capture, not a guess. |
| `412` / `417` | evdev 524 / 556, unnamed | retired 2026-08-22 — CEA-2014-A *web* keyCodes for RW/FF, absent from 336 real presses |
| `33` / `34` | `SDL_SCANCODE_4` / `SDL_SCANCODE_5`, evdev 5 / 6 | retired 2026-08-23 — §4.1 |

---

## 3. Three asymmetries that are behaviour, not untidiness

**PAUSE and PLAY are matched in `wcode` ALONE; their alternates, and STOP, in either field.** That
is how the two `app.rs` arms spelled it before `classify` existed, and it was preserved verbatim.
Widening or narrowing one of them is a behaviour change and needs its own argument — the codes come
from two different namespaces (§1, §4), so "make them consistent" is not the tidy-up it looks like.

**`Left`/`Right` carry an `alt` flag, and the two horizontal tests accept different sets.** `alt`
is `false` for a press that arrived as the plain `SDLK_LEFT`/`SDLK_RIGHT` sym and `true` for one
that arrived only as `WCODE_REWIND`/`WCODE_FASTFORWARD`. The non-player four-way nav dispatch
matches `alt: false` alone, so a transport key does nothing on Home; the player's scrub arm and the
Chapters strip match both. A press carrying BOTH stays plain — `classify` asks `sym` first — which
is what keeps Home's navigation answering an ordinary arrow that happens to ride beside a transport
code.

**`Key::Other` is not a synonym for unbound.** The Library pager is a separate predicate
(`page_dir`), Search's edit keys are read inside the screen, and the PIN keypad reads digits: all
three classify as `Other` and are then handled. Making the variant inert would break every one of
them. `is_bound` (§6) is the predicate that answers *is this press one the app binds at all*.

---

## 4. Namespace errors: the same bug, four times

`wcode` is a native scancode (§1). Several constants in this app were instead taken from the
**CEA-2014-A / LG web keyCode** namespace — the one LG's *web* app documentation describes, which
is a different world from a native SDL binary. Two are retired; two are open and recorded.

### 4.1 `33` / `34` — the channel rocker that was the digits (FIXED 2026-08-23)

`WCODE_CH_UP` / `WCODE_CH_DOWN` = 33 / 34 were bound in `page_dir` and described as the Magic
Remote's CH▲/CH▼ rocker, carrying the caveat *"verify the raw wcodes in the event log on a new
remote"* — a caveat nobody ever spent. In the web keyCode namespace 33/34 really are
ChannelUp/ChannelDown (they are the browser's PageUp/PageDown). In the **native scancode** namespace
this app receives, 33 and 34 are `SDL_SCANCODE_4` and `SDL_SCANCODE_5`, produced by evdev 5 `KEY_4`
and evdev 6 `KEY_5`.

**So pressing `4` or `5` on a number pad paged the Library grid.** Reproduced end to end on the
simulator before the fix — the digit `5` moved the grid focus a full page, and left every later key
in the script acting on a different item. The real rocker, 300/301, was already bound beside them,
so the two constants are **deleted**, not repointed: a constant that silently changes value is how a
doc or a log predating the change comes to read as the opposite of what it says. `page_dir` now
answers `SDLK_PAGEUP`/`SDLK_PAGEDOWN` and 300/301 and nothing else.

Graded three ways: `page_dir(digit) == None` and `is_bound(digit) == true` in `ui::consts`' host
tests, and the `k:53,34` row of `tests/keytable.json`, which is inert on all three recorded screens.

### 4.2 `19` as an alternate PLAY — OPEN, not changed here

`WCODE_PLAY_ALT_A = 19` is matched in **either** field. In `wcode`, **19 is `SDL_SCANCODE_P`**,
produced by evdev 25 `KEY_P`. A USB keyboard attached to the television would therefore start
playback on the letter `p`; on the Search screen, where printable characters arrive separately as
`SDL_TEXTINPUT` and the key event falls through to the ladder, typing `p` would plausibly leave the
screen for the player.

Left alone deliberately. It is a real finding of the same class as 4.1, but the alternate sets are
carried as an invariant in `docs/ui-viewtree-plan.md` §C, changing them is a documented behaviour
change rather than a tidy-up, and the sibling codes (`402`, `415`, `413`) come from evdev entries
`linux/input.h` does not name — so unlike `33`/`34` there is no positive evidence about them in
either direction. **Settle all four together, with the §7 capture plus a USB keyboard, in one pass.**

### 4.3 and 4.4 — `SDLK_SELECT`, and `digit_of`'s `wcode` arm

The other two are written up in §9, whose first and third bullets they are: `SDLK_SELECT` is
`SDL_SCANCODE_END`, and `profiles::digit_of` reads ASCII digits out of the scancode field. Both are
misreadings of the same field, both are recorded rather than changed, and the reasons are there.
`is_bound` (§6) declines to inherit the second of them.

---

## 5. HOME (#36) and LIVE (#39) — analysis, and what a device must still answer

**Nothing is bound, and nothing should be bound on the evidence available.** "HOME appears nowhere
in the tree" is not itself an argument; these three facts are.

**(1) The app can only ASK for two keys, and neither is HOME or LIVE.** LG's fork gates key
delivery behind an access policy the app opts into with an environment variable set before
`SDL_Init` — `src/main.c` sets `SDL_WEBOS_ACCESS_POLICY_KEYS_BACK`, which is why BACK reaches us at
all. The complete set of those hints, read out of the television's own binary with
`strings -a … | grep ACCESS_POLICY`, is **three**:

```
SDL_WEBOS_ACCESS_POLICY_KEYS_BACK
SDL_WEBOS_ACCESS_POLICY_KEYS_EXIT
_WEBOS_ACCESS_POLICY_FORCESTRETCH
```

(the last two also appear without the `SDL_` prefix — those are the shell-surface property names
the fork forwards). `SDL_webOS.h` in the NDK sysroot declares exactly the same two
`SDL_HINT_WEBOS_ACCESS_POLICY_KEYS_*` macros and no others. **There is no HOME hint and no LIVE
hint**, so a native app has no way to request either. A key with a request switch is one the
platform will hand over if asked; a key with none is one the platform keeps. HOME and LIVE are in
the second group by construction.

**(2) The fork CAN produce a HOME scancode, which is what makes this worth testing rather than
assuming.** The table at `0x92840` maps evdev 172 `KEY_HOMEPAGE` to **269**
(`SDL_SCANCODE_AC_HOME`). So "269 never arrives" is a claim about the compositor and SAM, not about
SDL — and it is exactly the claim a capture settles.

**(3) LIVE has no identified code at all.** Nothing in this project has ever recorded which evdev
code LG's *LIVE TV* / *Live Menu* button emits. The table can produce several plausible candidates,
and **`KEY_TV` (evdev 377) maps to 0, i.e. is not delivered at all** — which is enough to show that
guessing from an evdev name would be wrong. Binding anything for LIVE today would be a guess.

**A consideration that outranks all three.** Even if 269 arrived, it is not obvious the app *should*
act on it: HOME and LIVE are the television's own navigation, and an app that intercepted them would
be taking over the set's global controls, which is the opposite of what the checklist asks. So the
bar is not "can we receive it" but **"does LG's Native SDK, or a Seller Lounge QA requirement, say
the app is expected to receive it"** — and nothing consulted here says so. Public web-app Home/Back
documentation is QA context for the *requirement* and never proof about native SDL delivery.

**Verdict: leave unbound. #36 and #39 need one device observation, not code.** That observation is
§7, run once with HOME and LIVE among the buttons pressed. Until then neither row is markable, in
LG's own sense — "we never tested it" is not a markable state.

---

## 6. The unconsumed-press invariant (#40)

**A press consumed by neither a route-specific handler nor the global key ladder must produce no
global side effect.**

The hazard was structural rather than a missing arm. `app.rs::begin_fresh_press` runs for **every**
fresh press, *before* the ladder decides whether anything takes it, and two of the things it does
belong to no arm:

* `hud.note_fresh_press(now)` — un-dismisses the player HUD, so an unsupported key raised the
  transport over a film the viewer was watching;
* the armed-click abort — so an unsupported key cancelled a tvOS press in flight.

Both are now behind **`ui::consts::is_bound(sym, wcode)`**, which is §2's map expressed as one
predicate: `classify` names a `Key`, **or** `page_dir` answers, **or** the sym is backspace/Clear,
**or** the **sym** is an ASCII digit — **or**, in a Lab Diagnostics build only, the press is that
build's configured upload trigger (`crate::lab::is_trigger_key`, `false` at compile time in every
build anyone can install; `docs/lab-diagnostics.md`). The guarded half lives in
`app.rs::note_global_press`, split
out so `make check` can grade it — `begin_fresh_press` itself calls `hide_cursor`, a webOS-only SDL
symbol, and a host test that reaches it fails at `ld` rather than at an assertion.

**The digit term reads `sym` and not `wcode`, diverging from `profiles::digit_of`, on purpose.**
48–57 in the scancode field are `]` `\` `#` `;` `'` `` ` `` `,` `.` `/` and CapsLock (the digits'
scancodes are 30–39, which is how 33/34 turned out to be `4` and `5` in the first place). Mirroring
`digit_of` would have put the retired mis-reading straight back inside the gate that exists to stop
it. The consequence is that `is_bound` is not a strict superset of `digit_of` for a wcode-only
punctuation press — which is harmless, because the PIN pad has neither a HUD nor an armed click.

Three things stay unconditional, each for its own reason. `held.down_sym` is bookkeeping about the
physical key: without it, a held unsupported key's auto-repeats would each arrive as a fresh press.
The D-pad cursor gate is already narrower than `is_bound` — it takes the four plain direction syms
and nothing else. And the caller's `last_input` stamp is a local, read only by arms that run in the
same loop iteration, so an unbound press cannot carry it anywhere.

**The one place it deliberately over-approximates.** The digits count as bound *everywhere*, because
the who's-watching PIN keypad types from them, so a number press on Home or during playback still
un-dismisses the HUD. Narrowing that means making the predicate route-aware, i.e. a second copy of
the ladder's own order — the duplication `page_dir` exists to have ended. A number key is also,
unlike a colour button, a key this app really does bind, so calling it bound is honest.

Graded twice, because neither half implies the other: `app.rs`'s `unsupported_key_tests` covers the
two global effects (invisible to any focus fingerprint), and `tools/keytable.py` covers "moves
nothing on any screen" — its `k:0,269` (HOME) and `k:53,34` (the digit 5) rows are inert on Home,
Library and Search in `tests/keytable.json`.

**What neither covers**: the player route. The simulator has no video path, so no fingerprint in
that table was taken with a HUD on screen. The HUD half of this invariant has been graded as a unit
test and never on a television.

---

## 7. The recipe: capture a remote's real scancodes

**This is the only way to learn what any remote sends, and it needs a television.** An IR remote's
codes are not derivable from anything on a desk: the table in §1 says what the fork *can* produce
from a given evdev code, never which evdev code a given physical button emits.

The app logs every keyboard event's raw bytes unconditionally, as
`[<ticks>] key type=0x300 raw=<48 bytes of hex>`.

1. Take the television's lock (`tv-lock` skill) and wake it (`wake-tv`).
2. `make FLAVOR=debug deploy && make FLAVOR=debug run RUN_SECS=180`
3. Press **every physical button** on the remote under test, slowly, one at a time, writing each one
   down as you go — the log gives you codes in order and nothing else, so that order is the only
   label they will ever have.
4. Pull `make -s print-eventlog FLAVOR=debug` and decode each line's `+20` (`wcode`) and `+24`
   (`sym`) as little-endian `u32`.
5. Do it **once per remote**: the **Magic Remote** (done 2026-08-22, 336 lines) and a **standard IR
   remote** (checklist #26, never done — everything in this file has only ever been driven by the
   Magic Remote, which is exactly why that row is not a formality).

Rehearse the *handling* of any code you find against the simulator first. The remote-FIFO token
**`k:<sym>,<wcode>`** presses an arbitrary pair — the only way to drive a key the mnemonic token map
does not name, and therefore the only way to exercise an unsupported key headlessly at all.

Four things one run should settle: **HOME** (does 269 arrive, §5), **LIVE** (what code, if any, §5),
whether **461** and **412 / 417 / 413 / 415 / 402** ever fire (§2, §4.2), and the whole of #26.

---

## 8. BACK on the entry page — issues #16-#18 implemented 2026-09-03, one root still open

**This section recorded a known QA blocker for three weeks; three of its four roots are now
implemented and the fourth is named at the bottom.** LG's submission UX rules include, as quoted in
`docs/distribution.md` §2, that

> every selectable element must respond to 4-way + OK + Back, and on webOS 23–25 Back on the entry
> page must show the Home screen.

That is `distribute/*` submission and QA policy, so it is **presumed to apply to native apps** and
cannot be waved off as web-app-facing the way an API page under `develop/*` can. LG's `develop/*`
back-button guide says the same thing from the platform's side, and for the firmware this app
actually runs on: at an app's entry page, `webOS.platformBack()` "displays a popup asking whether to
exit the app on webOS TV 6.0 or higher, or **the Home launcher is launched on webOS TV 5.0 or
lower**" (<https://webostv.developer.lge.com/develop/guides/back-button>). The dev set is 4.10.2.

**What the app does today:** BACK at a ROOT — Home's root, the who's-watching picker's root, the QR
sign-in — hands the screen back to the television and keeps running (`app.rs::back_at_root` →
`webos::go_home`, which asks SAM to launch `com.webos.app.home` and falls back to minimizing the
surface). It does not ask, and it does not quit. The remote's own EXIT key still terminates
(checklist #38), and a script that wants the app closed uses SAM's `closeByAppId` exactly as
`make kill`, `tests/run.py` and `tools/tv-session.sh` already do — which is why the
`/tmp/plxnative-noexitconfirm` bypass went away with the "Exit PlxNative?" alert rather than being
kept: it existed only to let a caller quit by pressing BACK, and BACK is no longer a quit for
anybody.

**What replaced the confirmation is the platform's own behaviour, not its absence.** The alert was
added deliberately on 2026-08-21 and the warning that stood here — do not delete it on the strength
of the submission quotation alone — was right: what closed this is the `develop/*` statement of what
the platform DOES at an entry page on webOS ≤5, which makes "show the Home screen" a behaviour to
reproduce rather than a rule to obey by quitting.

---

## 9. Open, and worth knowing before trusting this file

* **The COLOUR buttons — MEASURED ON THE DEV SET, 2026-08-26.** They are `wcode` **486 RED, 487
  GREEN, 488 YELLOW, 489 BLUE**, with `sym` **0**, i.e. LG's private range and matched in `wcode`
  like every other remote button above 300. Provenance in the translation table: evdev 289/290/291/
  292 -> 486/487/488/489.

  Two things about that are worth carrying, because both would have sent a search in the wrong
  direction. **The standard evdev colour codes are a dead end on this firmware**: `KEY_RED` (398),
  `KEY_YELLOW` (400) and `KEY_BLUE` (401) map to **0 — not producible**, and `KEY_GREEN` (399) maps
  to 504, which is a code **this remote never sends**. So the one colour key that looked answerable
  offline was answerable *wrongly*. And the four are contiguous, which no reading of the table
  predicts — 486-489 sit in a block with 480/481 (evdev 293/294) and 482 (BACK, evdev 303), not
  with each other by colour.

  The app still binds none of them by default (checklist #40 wants them inert), and the presses
  above confirmed that: four presses, four `key type=0x300` lines, nothing moved. `crate::lab`
  binds ONE of them in a lab build — `docs/lab-diagnostics.md` §7 — and it is configuration, not a
  constant, because this measurement is one remote on one firmware.

* **`SDLK_SELECT` is misnamed: `77 | 1<<30` is `SDLK_END`.** `SDL_SCANCODE_SELECT` is 119; 77 sits
  in the `INSERT`/`HOME`/`PAGEUP`/`DELETE`/`END`/`PAGEDOWN` run at 73–78, and the television's table
  confirms it — entry 107 (`KEY_END`) is what produces 77. So `is_ok` accepts a keyboard's **End**
  key and would NOT accept a remote's real SELECT. Left as-is: the name is read from
  `ui/profiles.rs`'s test, and repointing the value would make `is_ok` answer a different key on no
  evidence that any remote sends 119 either. OK itself is unaffected — the remote's OK is
  `SDL_SCANCODE_RETURN`, captured and bound.
* **§4.2** — `19` is `SDL_SCANCODE_P` and is bound to PLAY. Untested on a device; needs a USB
  keyboard as well as §7.
* **`ui::profiles`' `digit_of` reads the digit range in `wcode` too**, where 48–57 are not digits at
  all but `]` `\` `#` `;` `'` `` ` `` `,` `.` `/` and CapsLock — and its own test asserts
  `digit_of(0, 55) == Some(b'7')`, where scancode 55 is a full stop. No remote has those keys, so it
  is harmless in practice and `profiles.rs` is left alone here; a USB keyboard on the PIN screen
  would type digits from punctuation. `is_bound` deliberately does **not** mirror it (§6). The same
  capture settles both.
* **`461` and `156` are not producible from the evdev table.** 156 is explained — the panel's *Clear
  all* comes through the fork's text path, and is device-measured. 461 is not explained: it is a
  defensive second BACK code with no evidence behind it on this firmware.
* **The `Provenance` column is about ONE firmware**, the dev set's (webOS 4.5,
  `libSDL2-2.0.so.0.4.1`). The inventories `tools/fwcompat.py` reads are symbol lists and can say
  nothing about a table in `.rodata`; comparing another release means harvesting its `libSDL2`.
* **The simulator's key path is not the television's.** On the host, a synthetic press carries its
  `wcode` in `SDL_Keysym.mod` under a marker in `windowID`, because macOS `libSDL2` is sdl2-compat
  forwarding into SDL3 and the fields it preserves across `SDL_PushEvent` are not the obvious ones —
  `keysym.scancode` and `keysym.unused` are both discarded (`decode_key`'s doc has the measured
  table). Until 2026-08-23 the carrier was `unused`, so **every wcode-only FIFO token was silently
  dead in the simulator** and three rows of `tests/keytable.json` recorded "did nothing" for presses
  that never arrived. Anything read off `make sim` about *which* code arrives is a fact about that
  shim, not about a remote.
