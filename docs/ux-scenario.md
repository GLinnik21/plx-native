# UX Scenario — PlxNative

**App:** PlxNative · **id** `com.beb.plxnative` · **version** 0.4.1 · `"type": "native"`
**Platform:** LG webOS TV, 1920×1080 UI, Magic Remote and standard IR remote
**Document status:** written 2026-08-23 against the tree at that date. Every behaviour below is
read out of the source, not remembered; where a claim could not be verified this document says so
rather than rounding up.

---

## 0. What this document is, and why LG asked for it

LG's **App Self Checklist** is the required submission document, and four of its items do not
describe a control themselves — they defer to this one:

| item | what it asks | where this document answers it |
|---|---|---|
| **#2** | main page behaviour | §4 (Home) |
| **#10** | every UI button works | §3 and §4 — every focusable control on every screen, with what OK does to it |
| **#42** | sound | **§7** — and the answer is that the item is N/A on its own precondition |
| **#45** | playback control, with the remark *"refer to UX scenario for movement details"* | **§6** — the transport model, in full |

Item 45 is the reason this file is written carefully rather than as a formality. Its remark makes
the UX scenario the artefact QA grades the transport against, and this application's transport
model is deliberately not the one a literal reading of #45 would produce. §6 states that position
and the grounds for it.

**Companion documents.** `docs/lg-self-checklist.md` records this app's status item by item —
including the four that genuinely fail. `docs/distribution.md` §2 covers submission itself.
This file is about *behaviour on screen*.

---

## 1. The remote, and every key the app acts on

The app is driven entirely by the remote. There is no mouse requirement, no keyboard requirement
outside text entry, and every screen is reachable with the four-way pad, OK and BACK alone.

### 1.1 How keys arrive

LG's SDL fork ships a **shifted `SDL_KeyboardEvent`**, so the app does not read `e.key.keysym`; it
reads raw bytes off the event — `+16` state, `+20` the webOS keycode, `+24` the sym
(`rust-modules/src/app.rs`, via `rd_u32`). State's low byte is pressed(1)/released(0) and bit
`0x100` marks auto-repeat. A single press can fill **both** the sym and the keycode field, so a
pure classifier resolves the pair to exactly one action
(`rust-modules/src/ui/consts.rs::classify`).

### 1.2 The key table

Scancodes below were settled 2026-08-22 against two independent sources: LG's own evdev→scancode
table inside the television's `libSDL2-2.0.so.0.4.1`, and 336 real key lines captured off the
remote. They are **native scancodes**, not the CEA-2014 web keyCodes that LG's web-runtime
documentation lists — a distinction that had four transport keys bound to the wrong namespace until
that date, where they had never once fired.

| key | code(s) | action |
|---|---|---|
| **D-pad Up / Down / Left / Right** | SDL scancodes 82 / 81 / 80 / 79 | move focus; on the player, LEFT/RIGHT scrub |
| **OK / Enter** | `SDLK_RETURN` (13), keypad Enter, `SDLK_SELECT` | activate the focused control; on the player, play/pause |
| **BACK** | 482, also 461 | step back one screen; see §2.2 |
| **EXIT** | 505 | terminate the app immediately (§2.3) |
| **Play** | 450, also 19 / 402 | resume |
| **Pause** | 72, also 415 | pause |
| **Play/Pause** (one key) | 261 | whichever of the two is needed |
| **Stop** | 120, 260, also 413 | leave the player (§6.4) |
| **Fast-forward** | 451 | seek forward |
| **Rewind** | 452 | seek backward |
| **CH ▲ / CH ▼** | 300 / 301 (also `SDLK_PAGEUP`/`PAGEDOWN`) | page the Library grid |
| anything else | — | swallowed as `Key::Other`; no screen acts on it |

**LEFT/RIGHT carry a flag.** A press that arrived as the plain direction sym is
`Left { alt: false }`; one that arrived *only* as a transport key (rewind/fast-forward) is
`alt: true`. Four-way navigation on the non-player screens matches `alt: false` alone, so pressing
REWIND on Home does not move focus sideways; the player's scrub arm matches both, so REWIND and
LEFT do the same thing there (`ui/consts.rs`, the `Key::Left` doc). This asymmetry is deliberate.

### 1.3 Pointer

The Magic Remote's pointer is supported everywhere as a peer of the D-pad: motion, hover, click and
the scroll wheel all reach the same handlers the keys do, and the remote reports pointer auto-hide
(keycode `0x1e4`) so the UI can drop back to D-pad focus cleanly. **Nothing in the app requires the
pointer** — an IR remote with only a four-way pad reaches every control.

---

## 2. Screen map, and the two ways out

### 2.1 The map

```
                    ┌─ (no session) ─→  Sign in  ──┐
  launch → splash ──┤                              ├─→ Who's watching ─→ [What goes on Home?] ─→ HOME
                    └─ (session) ──────────────────┘        (only when the roster has >1 profile)

  HOME ──── tab strip ────→ LIBRARY (one pill per library section)
   │  └───────────────────→ SEARCH
   │  └── profile chip ───→ Account menu (popover)
   │  └── press-and-hold ─→ Card context menu (popover)
   │
   └── OK on a card ──→ DETAIL ──→ PERSON (cast headshot) ──→ DETAIL ──→ …
                          │
                          └── Play ──→ PLAYER ──→ overlays: track menu · chapters · info
```

Home, Library and Search are **peers**: they share the top tab strip, and moving between them
cross-fades the page while the chrome holds still. Detail and Person **stack** — they have no tab
strip, so entering or leaving one fades the whole screen including the bar.

### 2.2 The BACK trail

BACK steps back through a real trail of visited nodes (`ui::trail`), not a hardcoded parent. So
`Home → Detail → Person → Detail` unwinds in exactly that order. Two consequences worth stating for
QA:

- **Search is a peer, not a page.** BACK from Search returns to Home. What Search *opens* stacks;
  Search itself does not.
- **BACK at Home's own root is the end of the chain.** It does not quit. It raises the app's one
  decision alert (§5.9).

### 2.3 EXIT

The remote's **EXIT** key terminates the app outright and does **not** raise the alert — the alert
exists so that BACK cannot quit by accident, and a key labelled EXIT carries no such ambiguity
(`app.rs`, the `Key::Exit` arm). Device-verified 2026-08-22: press → `EXIT key: terminating` in the
event log → the process is gone.

---

## 3. Focus, and what "every button works" means here

Every focusable control has three visually distinct states — **idle**, **focused** and
**pressed**. Focus is drawn as a scale-up on a critically damped spring plus a glow; press is a
separate depression (`ui/press.rs`). **Every control that performs an action responds to OK.**

Two focus stops do not, and both are deliberate rather than dead: the tab pill for the page you are
*already on* (OK on Home's own pill, standing on Home, is a documented no-op), and the person page's
header band when that person has no biography long enough to expand. Neither is a button, neither
leaves the user stuck, and every other focus stop in the app acts.

**A long press is its own gesture.** Holding OK for ≥500 ms on a card opens that card's context
menu (§5.6); releasing before then is an ordinary activation. Nothing else in the app uses a hold.

**Text entry uses the television's own keyboard**, raised by `SDL_StartTextInput`. The app draws no
keyboard of its own. This is the system VKB in LG's Wayland driver; it opens and re-opens correctly
on the dev set (photographed 2026-08-14). Its own edit keys — the caret arrows, delete and **Clear
all** — arrive as ordinary key events and are all handled, so no button on the panel is inert.

**Text that does not fit is handled, never clipped** (checklist #6). Short strings elide with an
ellipsis at a measured boundary; long-form text — a synopsis, a biography — gets a scrolling text
view rather than a truncation. All text is rendered at whole-pixel origins with light hinting, which
is what keeps stems and bars at their intended weight on a 1080p panel.

---

## 4. Home — the main page (checklist #2)

![Home, hero](screenshots/ux-home-hero.jpg)

Home opens on a **hero** — one item, full-bleed backdrop, with its logo or title, a one-line fact
row (kind · year · rating), a truncated summary and an action row. The hero rotates through a set of
promoted items; the dots under the action row show the position.

**Controls on the hero row**, left to right:

| control | what OK does |
|---|---|
| **Play** / **Continue** | starts playback — *Continue* when the item has a resume point, *Play* when it does not |
| **ⓘ** (info) | opens that item's detail page |

Those are the only two. **The `›` beside them is an indicator, not a third button** — it is drawn,
it never takes focus and it has no hit rect. The hero is paged by **RIGHT past the info disc** or
**LEFT off the Play pill**, and it also advances itself every **8 s** while the row sits idle. The
dots under the row show the position.

**Above it**, the top tab strip: **Home**, then **one pill per library section the server reports**
(here *Movies* and *TV Shows*), then the **Search** pill — drawn as a magnifier mark, not a word.
The strip is uncapped and scrolls once it outgrows the bar, so every discovered section is
focusable. At the left margin, outside the group, sits the **profile chip** (§5.7).

**UP from the hero reaches the bar**, landing on the profile chip; LEFT and RIGHT then walk the
whole band — **chip → Home → Movies → TV Shows → … → Search**. DOWN leaves the bar: back to the
hero action row from the chip, or into the shelves from a pill. The bar is worn by Home, the Library
and Search and by nothing else — the detail, person, player and onboarding screens have none, which
is why moving to or from them fades the whole screen rather than just the page.

**Below the hero**, the shelves.

![Home, shelves](screenshots/ux-home-shelves.jpg)

DOWN from the hero enters the shelves; DOWN again steps between them; LEFT/RIGHT walks the cards.
The focused card grows, and its title and a status line appear beneath it — here *"2 hr 6 min
left"*, which is the resume state. A watched item carries a check mark in its corner.

**OK on a card in *Continue Watching* plays it directly** — as does OK on any card that is itself an
episode, wherever it sits. **OK on any other card opens that item's page.** The difference is
intentional: the deck is a list of things already in progress, and one press is the whole point of
it.

One refinement inside the deck rule: a **show** or **season** card there has no single stream to
start, so it opens its page, waits for the load to land on the expected item, and fires that page's
Play only then — never blindly, because a failed fetch would otherwise play whatever page was open
before. On any other shelf, a show or season card simply opens its page.

**BACK** from the shelves returns focus to the hero row; BACK at the hero row raises the exit alert
(§5.9).

**Overscan.** The UI is authored at a fixed logical 1920×1080 with a 90 px horizontal margin
(4.7%), inside LG's 5% overscan frame; nothing load-bearing is drawn outside it.

---

## 5. Every other screen

### 5.1 Splash

A 1920×1080 PNG (`splash.png`), shown by the platform while the app starts. Its black point is
lifted and its ground is the app's own surface colour, so it matches the first frame the app itself
draws rather than cutting to it. (LG's guidance is explicit that the splash should not be black.)

### 5.2 Sign in

![Sign in](screenshots/ux-signin.jpg)

Reached automatically when there is no usable session, and on demand from the account menu. It shows
a **QR code** and a **four-character link code**, with the instruction to scan it or go to
`plex.tv/link` and enter the code. A spinner reads *"Waiting for you to sign in…"* and the screen
advances by itself the moment the account is linked — there is nothing to press.

*The code in the figure is a one-time, short-lived claim code. It names no account.*

Account **creation** happens on plex.tv, not in the app.

### 5.3 Who's watching

Shown after sign-in when the Plex Home roster holds more than one profile.

Centred title **"Who's watching?"**, a horizontal row of **circular profile avatars** with each
name beneath, and a **Sign out** pill centred below them.

| key | effect |
|---|---|
| LEFT / RIGHT | walk the roster — clamped at both ends, no wrap |
| DOWN | move to the **Sign out** pill |
| UP | back to the roster |
| OK | commit the focused profile, or sign out if the pill has focus |
| BACK | resume the persisted session and go to Home (swallowed when there is no session behind the picker) |

An empty roster draws a spinner; a failed switch draws an error line under the row.

**A protected profile opens a PIN pad** rather than committing. The pad is its own screen, not a
peek-through: a title `Enter <Name>'s PIN`, four entry dots, and a 4×3 keypad —
`1 2 3 / 4 5 6 / 7 8 9 / · 0 ⌫`, the bottom-left cell empty and unfocusable. The D-pad walks the
grid and skips the empty cell, OK presses a key, and **the remote's own number buttons type straight
into the PIN**. On the fourth digit the pad stays up and the dot row becomes a spinner while the PIN
is verified; only BACK acts during that. A wrong PIN pulses the dots red for about 1.4 s and
restarts entry **on the same pad** rather than dumping the user back to the roster. BACK closes the
pad back to the roster.

> **Figure deliberately omitted.** The roster on any real account is a list of real people's names.
> A screenshot of it is not this project's to publish. QA will see the screen on the bench, and the
> submission copy of this document can carry a capture from a purpose-made account.

### 5.4 Library

![Library grid](screenshots/ux-library-grid.jpg)

Reached from any library pill in the top strip. A poster grid, four-way navigable, with:

- **Sort** and **Filter** pills above the grid (left);
- the item **count** (right) — *"27 films"*;
- an **A–Z rail** down the right edge, which jumps the grid to a letter;
- watched check marks on the cards.

**CH ▲ / CH ▼ page the grid**, which is the fast way through a large library.

**BACK here is two steps, not one**: from the grid or the toolbar it returns focus to the tab row,
and only a second BACK leaves the library for Home. UP from the toolbar does the same thing, landing
on the pill of the library being browsed.

Sort and Filter open as **popovers over the live grid**, never as full-screen sheets:

![Library sort menu](screenshots/ux-library-sort.jpg)

The options are the ones the **server** reports for that section, not a hardcoded list; the current
choice carries a check and a direction chevron, and OK on the current choice reverses the direction.
BACK closes the popover and returns focus to the pill.

### 5.5 Search

![Search](screenshots/ux-search.jpg)

The last pill in the top strip. The screen is a vertical stack of zones — the shared top bar (the
profile chip and the pills), the **field**, and under it either the **recent searches** or the
**results**.

**OK on the focused field toggles editing**: it raises the **television's own on-screen keyboard**
if the field is not being edited, and commits the term if it is. A pointer click on the field always
raises the keyboard, because a click *is* the request to type. The app draws no keyboard of its own;
this is the system panel, and its own edit keys — the arrows, delete and **Clear all** — are handled
as ordinary keys, so every button on it does something.

Results arrive as you type and are grouped into one shelf per kind (*Movies*, *TV Shows*, …) with a
result count beside each heading. UP/DOWN moves between the zones and between shelves, LEFT/RIGHT
walks a shelf, OK opens the item's detail page. ▲ from the field reaches the top strip; ◀ off the
first pill reaches the profile chip. **BACK returns to Home** — Search is a peer, not a page (§2.2).

The line beside the field names the **scope** — which server or servers are being searched. Search
covers the user's own servers and any shared with the account; Plex Discover / Watchlist catalog
results are deliberately out of scope, by decision rather than by omission.

*The scope line in the figure is redacted: it names a real server. The substitute is the app's own
string for a server that reports no name (`ui/search/field.rs::scope_text`), so the figure shows a
state the app really produces.*

### 5.6 Card context menu

![Card context menu](screenshots/ux-item-menu.jpg)

Opened by **pressing and holding OK on a focused card** for ≥500 ms — a popover anchored to the
card, over the live page. UP/DOWN moves, OK activates, BACK closes. The actions are the ones that
apply to that item; for an in-progress movie on the deck:

*Go to Movie* · *Mark as Watched* · *Mark as Unwatched* · *Play from Start* · *Remove from Deck*.

### 5.7 Account menu

![Account menu](screenshots/ux-account-menu.jpg)

The **profile chip** at the top left. Focusing it expands the chip to show the current identity;
OK opens a popover headed *ACCOUNT*. Signed in, it offers switching profile and **signing out**
(checklist #21); signed out, it offers **Sign in**, which is the figure above.

One more row, **Settings**, is offered in **every** state, signed in or out. A person who cannot get
past sign-in has still received a copy of this software, so privacy and legal information cannot be
conditional on an account. *(The figure above predates this row.)* Settings opens a full-screen
modal: signed-in users also get **Home screen**, while everyone gets **Privacy & data**, **Legal
notices** and **About PlxNative**.

- **Privacy & data** holds the two saved reporting choices, one scrollable what-is-actually-sent document PER CHANNEL (Crashes / Errors, Analytics / Usage),
  the privacy policy, and Delete all local data. Changed switches commit through Done; BACK
  discards the draft. Playback diagnostics remain a playback action rather than a global setting.
- **Legal notices** contains six readable documents: **Privacy policy**, **Open-source licences**,
  **FFmpeg & source offer**, **PlxNative source code**, **Trademarks & non-affiliation**, and
  **Privacy & security contact**. UP/DOWN scrolls a notice; BACK leaves it.

### 5.8 Detail

![Detail page](screenshots/ux-detail.jpg)

Opened by OK on a card (or the hero's ⓘ). Full-bleed backdrop, title treatment, a fact row
(kind · genres · certificate, with **4K** and **CC** badges where they apply), the critic/audience
ratings, the summary, and a technical line: release date · runtime · **and the playback verdict for
this item on this television** — *"Direct Play"* or *"Converts on server"*.

The action row:

| control | what OK does |
|---|---|
| **Play** / **Resume** | start, or resume from the stored position |
| **↺** | play from the start (shown when there is a resume point) |
| the watched disc | toggle watched — it **wears the face of the write it would perform**, so it is a **✓** on an unwatched item and a **−** on a watched one |

Below, **Cast & Crew** — OK on a headshot opens that person's page, from which their other titles
lead back into more detail pages. The BACK trail unwinds the whole chain.

For a series, the page carries its seasons and episodes rather than a single action row.

### 5.9 Exit alert

![Exit alert](screenshots/ux-exit-alert.jpg)

Raised by **BACK at Home's root** — the app's only decision alert. Two buttons: **Cancel**, focused
by default and drawn in the affirmative face, and **Exit** in the destructive face. LEFT/RIGHT
chooses, OK commits, BACK cancels.

Before 2026-08-21 BACK at Home quit on the press. The alert exists because that is a lot of state to
discard on one keystroke and there was no undo.

### 5.10 Failure read-out

![Playback failure](screenshots/ux-failure.jpg)

When playback cannot start, the app does not fail silently and does not show a spinner forever. It
draws a full-screen read-out: a warning mark, a headline, **the reason in plain language**, the
server's own verdict beneath it where there is one, and the way out — *"Press BACK to return"*.

The figure is a real server verdict (*"Cannot convert this item. Implementation for video encoder
'hevc' not found."*). The screen is shaped to survive being photographed off a panel and pasted into
a bug report, which is the state it is usually seen in.

### 5.11 Diagnostics read-out

*(No figure yet, and deliberately not a simulator one: on a Mac every hardware row reads "unknown"
because there is no nyx and no codec table to read, so a picture of that state would present the
panel's failure mode as its normal one. This figure is owed from a television.)*

A panel the viewer turns on, photographs and turns off — the app's answer to "playback does not work
on my set" from hardware nobody here owns. Its user-facing entry point is during playback,
**More → Stats for nerds** (§6.3). Automated development captures may arm it directly on another
route, but the profile menu does not expose a playback instrument as a global setting.

It takes **no buttons at all**: every key keeps working underneath it, which is what lets you watch
the numbers move as you press play, and it turns off by picking the same row again.

What it shows depends on whether anything has been asked to play. **During or after a playback** it
is the pipeline: the source and the server, the codec chain from your file through your server to
what was declared to the decoder, the video plane, how far Load got, whether the demuxer produced
anything, what was fed and whether it was accepted, HTTP status, and the adaptive controller's own
inputs — with three sweep plots for budget versus demand, network activity, and buffer health.
**Before anything has played** it is the set instead: model and board, the firmware
codename, what the decoder claims *and whether that table was actually readable*, and whether the
server ever answered. Those are the facts a report needs when the app opens and finds nothing, which
is the failure that never reaches a player at all.

Nothing on it names a title, a person, a library, a server or an address, and no value is ever a URL
or a path — a photograph cannot be redacted after the fact, and it lands in a public issue thread.

---

## 6. Playback control — the answer to checklist #45

### 6.1 The position, stated plainly

**There is no on-screen `<< ⏸ >>` transport button row in this application, and one is not
planned.**

Transport is driven from **the remote**, and the HUD shows **state**: a small mark beside the
elapsed clock, and nothing at all while playback is running steadily.

This is a design decision, recorded and settled — not an oversight, and not a gap awaiting work. The
grounds are below, and they are the reason this document exists.

### 6.2 Why this is compliant

1. **The keys are what #45 is actually about.** The item's own remark defers movement details to
   this document; what it must be satisfied about is that Play, Pause, Stop, fast-forward and rewind
   *work*. They do, from the remote's dedicated keys. Those bindings were settled on 2026-08-22
   against two independent sources — LG's own evdev→scancode table inside the television's
   `libSDL2-2.0.so.0.4.1`, and 336 real key lines captured off the remote (§1.2) — replacing four
   codes that had been bound to the **web-runtime** keyCode namespace and had therefore never once
   fired. That is the half that must really work, and it is the half a button row does not affect.
2. **On-screen playback controls are RECOMMENDED, not mandatory.** In the checklist document itself
   they sit under the *Recommended* heading rather than the mandatory one. An app is not rejected
   for the absence of a recommended control; it is rejected for a mandatory one that fails. *(Read
   off the submitted checklist's own section headings — verify against the copy being submitted, as
   the checklist is versioned.)*
3. **The idiom is already shipping on this platform, approved.** Apple's TV application is a
   Content Store app on LG webOS sets and uses the Apple-TV transport idiom — remote-driven, with a
   scrubber and a state read-out rather than a persistent focusable `<< ⏸ >>` row. *(Tier: observed
   behaviour of a third-party shipping app, not an LG statement. It is offered as precedent that the
   idiom passes QA on these sets, not as a rule LG has written down.)*
4. **Every control the app does draw is fully operable by four-way + OK + BACK**, which is the hard
   UX rule (§3). Nothing anywhere in the app is reachable only by the pointer. (The one gesture in
   the app is the card hold of §5.6; its actions are otherwise reachable from the detail page, with
   the single exception of *Remove from Deck*.)

### 6.3 The HUD

![Player HUD](screenshots/player.jpg)

*Device capture. The Starfish/ACB media seam exists only on the television, so the player cannot be
photographed on the desktop simulator — see §9. The figure shows the **paused** state: note the
pause mark immediately right of the `0:25` clock, and that there is no transport button row.*

The HUD is summoned by any input and hides itself again after **4.5 s** of no input — **8 s** while
one of its panels is open, which is longer read time for a list. While it is hidden the picture is
completely unobstructed: the app draws nothing at all over the video plane.

What it contains, over a scrim that fades up from the bottom of the screen:

- **the title**, left, with an episode kicker above it for a series (*"S24, E4 · Bringing Up Brady"*
  over *Family Guy*);
- **the scrubber** — a full-width bar with a playhead knob that grows and glows when it has focus;
- **the elapsed clock**, which travels under the knob, and the **remaining clock**, right-aligned —
  the remaining clock hides itself if the travelling elapsed label would collide with it;
- **the state mark**, next to the elapsed clock — §6.5;
- the **Info** / **Chapters** tabs, bottom left;
- the **control row**, top right — §6.7.

Both clocks are laid out on a `0`-digit template so the numbers do not wobble as they change.

**The scrubber rail is deliberately bare** — no chapter ticks, no marker bands. Both were built and
removed.

**Focus inside the HUD is three bands.** UP and DOWN walk them in this screen order:

| position | band | LEFT/RIGHT there |
|---|---|---|
| top | the **control row** | move between its items |
| middle | the **scrubber** | seek (§6.6) |
| bottom | the **Info / Chapters** tabs | move between them (*Chapters* is present only when the item has chapters) |

Two edges are worth knowing: **UP from the control row hides the HUD** — there is nothing above it,
and dismissing is the useful answer — and **DOWN from the tabs does nothing**, since they are the
bottom. Leaving the scrubber cancels any seek preview that was in progress rather than committing it.

Everything in the control row and the tab row is focusable and activates with OK. **The state mark
is not focusable and is not hit-tested** — it is a read-out, not a control, which is exactly the
distinction §6.1 draws.

### 6.4 What each key does during playback

| key | effect |
|---|---|
| **OK** | on the scrubber band, toggle play/pause; on a focused control, activate it |
| **Play** / **Pause** / **Play-Pause** | the corresponding transition, from anywhere |
| **LEFT / RIGHT** | seek, or move within the focused band — §6.6 |
| **Rewind / Fast-forward** | the same as LEFT / RIGHT (§1.2) |
| **UP / DOWN** | move between the HUD's three bands; a press on a hidden HUD is spent raising it |
| **Stop** | leave the player |
| **BACK** | leave the player |
| **EXIT** | terminate the app (§2.3) |

**Stop and BACK do the same thing here**: both run one teardown ritual — cancel any play still being
resolved, close every panel, stop the buffer feed — and return to **the page playback started
from**. For an episode that is the **show page, scrolled to the episode that played**, not the
generic show root. The final position is reported to the server on the way out, so the item resumes
where it was left, on this client or any other, and Continue Watching is refreshed shortly after.

While a playback **failure** owns the frame (§5.10), BACK is the only key that acts.

### 6.5 The state mark — what the HUD shows instead of buttons

One slot, beside the elapsed clock, showing exactly one of:

| state | mark |
|---|---|
| playing steadily | **nothing** |
| paused | pause mark |
| playhead travelling backwards (scrub, chapter hop, seek burst) | rewind mark, drawn to the **left** of the clock |
| playhead travelling forwards | fast-forward mark |
| just resumed | play mark, for **two seconds**, then it clears |
| waiting on the pipeline with no travel to show | inline spinner |

Precedence is travel → pipeline → paused → the resume mark, so a paused scrub still reads as a
scrub. Rewind sits on the clock's left and everything else on its right, so the mark points the way
the playhead is going.

Three properties of this design are the substance of the decision, and are the part worth putting in
front of QA:

- **The empty state is the common one, and that is the point.** A mark that is always up says
  nothing. This slot answers *did that press land* and *which way am I going* — questions a static
  button row cannot answer at all.
- **The direction is derived from the playhead, not from the keycode.** One rule therefore covers a
  LEFT/RIGHT scrub, a chapter or marker hop and a rapid seek burst — none of which agree about which
  key, if any, was pressed. It reports **net** travel, so dragging back from +100 s to +50 s while
  still ahead of the playhead keeps reading fast-forward: it answers *which way will I jump when I
  let go*.
- **The play mark clears after two seconds** deliberately. A play glyph held for a whole film would
  be saying "playing" to somebody who is watching a moving picture.

### 6.6 Seeking

Seeking is a **preview-then-commit** gesture, not one immediate jump per press.

- **With the HUD hidden, a LEFT/RIGHT press is spent raising it and moves nothing.** The HUD sits on
  a 4.5 s timer over full-screen video, so *"where am I"* and *"take me back ten seconds"* are two
  different intentions the remote cannot distinguish; acting on a band nobody can see is how a
  viewer glancing at the clock loses their place. **A hold is not affected** — holding LEFT raises
  the HUD and then runs the ordinary continuous scrub as the auto-repeats arrive, by which point the
  band being dragged is on screen.
- **With the HUD up and the scrubber focused, one press moves the preview 10 s.** The picture does
  not move yet; the scrubber shows the target, and the state mark shows the direction.
- **Holding engages a continuous scrub that accelerates** — from 10× up to 140× playback speed — and
  the key release commits it. A lost key-up is caught by a 400 ms watchdog, so a dropped release
  cannot leave the scrub running.
- **Quick repeated taps accumulate into one seek.** The commit waits ~450 ms after the last tap, so
  "back thirty seconds" is three presses and a single seek rather than three. That matters beyond
  feel: back-to-back in-flight seeks are what stress the demux pipeline.
- The seek target is clamped to the item, stopping 3 s short of the end.

The pointer can drag the scrubber directly, under the same commit rule.

### 6.7 The control row — subtitles and audio, and what replaces them

The row at the HUD's top right has **three mutually exclusive occupants**, decided by what is under
the playhead. It is one row, not three stacked controls, and the choice is made once per frame.

**1. Normally: three round discs — Subtitles, Audio, More (`…`).**

Each opens a **popover over the live picture** — the same idiom as the library's sort menu, never a
full-screen sheet. All three are modal while open: they take every key, and BACK closes them.

- **Subtitles** and **Audio** open the same two-panel track menu, on whichever panel was asked for.
  **LEFT/RIGHT swaps between the Audio and Subtitles panels**, UP/DOWN moves the row, OK selects and
  closes. Subtitles offers the item's subtitle tracks plus *Off*; both text and image (PGS/VobSub)
  subtitles render. Audio offers the item's audio tracks. Switching is a **server-side** selection,
  so it persists as the item's preference rather than lasting only this session.
- **More** opens an *Options* popover — one row today, **Stats for nerds**, an On/Off toggle that
  overlays live playback statistics. LEFT/RIGHT are deliberately swallowed here: it is one column.

The app exposes **no subtitle appearance settings**, and its client-rendered subtitles do not read
the television's own subtitle settings — stated here because that is what makes the second half of
checklist #48 N/A rather than failing.

**2. Over a marked segment: a Skip pill.** When the server has marked an intro or credits region and
the playhead is inside it, a single **Skip Intro** / **Skip Credits** pill takes the row — same
position, same height, never narrower than the disc pair, so the transport does not visibly jump.
When the segment begins the HUD is raised and the focus ring is parked on the pill, so **one bare OK
skips**. OK seeks past the segment and resumes; on a `final` credits marker it finishes the item
instead. The pill leaves with the segment, and focus returns to the scrubber when it does.

**3. Over the credits with another episode queued: Up Next.** The row becomes the next episode's
still, a caption (*"Up Next · S2, E4 · Laura"*), and **two buttons — Watch Credits** on the left and
**Next Episode** on the right, with the focus ring parked on *Next Episode*. A **10 s countdown**
runs as that button's own fill sweep and starts the next episode when it completes. The countdown
runs only while the transport is bare, the row holds focus and the ring is on *Next Episode* — any
other state cancels it, and **once cancelled it stays cancelled** for that segment. *Watch Credits*
cancels it explicitly and leaves the tile as a plain OK-to-play target. While the clock runs the HUD
is held up, so the countdown is never invisible.

Up Next deliberately outranks Skip Credits: with somewhere to go, "next episode" is the better
offer.

**Credits detection is a Plex Pass server feature, and where the server has none the app synthesizes
a 30 s tail — but only when there is a next episode to offer.** So the synthesized tail always
raises Up Next and can never raise a Skip pill: a movie's tail must not grow a *Skip Credits* button
pointing nowhere. The practical consequence for a bench test is that on a Pass-less server **Skip
Credits is unreachable**, and only *Skip Intro* and *Up Next* can be exercised.

At the end of an item with nothing queued, the player simply exits to the page it came from. **There
is no full-screen post-play interstitial** — one was built and is deliberately not shipped.

### 6.8 Info and Chapters

The two tabs at the HUD's bottom left. Both are modal panels on the live player, and BACK closes
either back to the plain HUD; **DOWN past the end of either drops focus back onto the tab row**,
which is the way out without BACK.

- **Info** — the episode still, title, synopsis and a metadata line with capability badges, beside a
  column of two actions: **From Beginning** and **Go to Show** / **Go to Movie**. UP/DOWN walks them,
  OK activates.
- **Chapters** — a horizontal strip of chapter cards, each a thumbnail with its name and timestamp.
  It opens focused on the chapter containing the playhead; LEFT/RIGHT picks, OK seeks there and
  resumes. The tab exists only when the playing item has chapters.

---

## 7. Sound — the answer to checklist #42

**The application produces no audio of its own.** Not reduced, not muted by default, not switched
off in a setting — none exists. This was checked five independent ways across the whole tree:

- the app initialises SDL with **`SDL_INIT_VIDEO` alone** — it is the only flag and the only
  `SDL_Init` call in the program (`rust-modules/src/app.rs`). SDL's audio subsystem is never
  started, so there is no path by which the app could emit a sound;
- **no SDL audio call exists anywhere** — no `SDL_OpenAudio`, `SDL_OpenAudioDevice`, `SDL_QueueAudio`,
  `SDL_LoadWAV` or `SDL_MixAudio`. The vendored `SDL_audio.h` header is never included;
- **`SDL_mixer` is not present at all** — no `Mix_*` symbol, no header, and it is not linked. The
  link line is SDL2, SDL2_ttf, GLESv2, luna-service2, glib, wayland-client and LG's two media
  libraries, and nothing else (`Makefile`, `LIBS_REAL`);
- **there are no audio assets** — no `.wav`, `.mp3`, `.ogg`, `.aac` or `.m4a` anywhere in the
  shipped tree. `assets/` holds a logo, a splash and the UI's SVG icon set, and nothing that decodes
  as audio;
- **no webOS sound service is called** — no `playSound`, no `com.webos.service.audio`, no key-tone
  or feedback Luna request.

So there is no background music, no navigation click, no focus tick and no alert tone, and no build
configuration in which one could appear.

**The only audio the television produces while this app is running is the audio track of the video
being played**, decoded by the TV's own media pipeline and mixed by the TV.

Consequently:

- there is **no in-app sound on/off control**, because there is nothing for it to switch;
- volume, mute and every audio output setting are the **television's**, reached with the remote's
  own volume keys, and the app neither intercepts nor overrides them;
- **item #42 is N/A on its own precondition.** It asks about the app's sound; the app has none.

Audio *of the video* is in scope and works: the app selects the audio track, declares its codec to
the television's decoder, and offers the track menu in §6.7.

---

## 8. Checklist cross-reference

Where the four deferring items are answered, and the neighbouring items this document also settles.

| item | answered in |
|---|---|
| #2 main page | §4 |
| #3 reboot behaviour | not a UX question — see `docs/lg-self-checklist.md` |
| #6 correct text | §3 (elision and long-form scrolling) |
| #7 focus / mouse-over states | §3 |
| #10 all UI buttons work | §3, §4, §5, §6.3 |
| #11 / #12 BACK and EXIT *UI buttons* | N/A — the app draws neither; the **keys** are §2.2 and §2.3 |
| #21 sign out | §5.7 (account menu) and §5.3 (the picker's own *Sign out* pill) |
| #26–33 remote, pointer, OK, wheel, navigation | §1 |
| #37 BACK key | §2.2, §5.9 |
| #38 EXIT key | §2.3 |
| **#42 sound** | **§7 — N/A on its own precondition** |
| #44 full/original screen toggle | N/A — no such control; video is always full-panel |
| **#45 playback control** | **§6** |
| #46 replay after completion | partly §6.7 (Up Next / end of item). **Not fully answered here** — see `docs/lg-self-checklist.md`, where it is a runnable case as of 2026-08-23 (`pipe_replay_after_eos`) awaiting its device run, rather than the untested item this row used to point at |
| #48 subtitles | §6.7 — rendering in scope, appearance settings N/A |
| #49 resume | §5.8, §6.4 |

---

## 9. Figures, and which ones still need a device capture

Every figure in this document except the player HUD was taken from the **desktop simulator**
(`make sim`), which runs the same application core, against a real Plex server, at the authored
1920×1080. Layout, focus, navigation and the whole data layer are identical to the television's.

**What the simulator cannot photograph** is anything involving video. The Starfish/ACB media seam is
29 symbols that exist only on the television, so a Play press on the desktop lands on the app's real
failure read-out — which is how §5.10's figure was taken honestly, and why the player figures cannot
be.

| figure | source | status |
|---|---|---|
| Home hero, Home shelves, Detail, Card menu, Library grid, Library sort, Search, Account menu, Sign in, Exit alert, Failure read-out | simulator, 1920×1080 | **in this document** |
| Player HUD (`screenshots/player.jpg`) | device capture | **in this document** — shows the paused state mark |
| Player HUD, playing steadily (empty mark slot) | device | **needed** |
| Player HUD, fast-forward and rewind marks | device | **needed** |
| Subtitle track menu · Audio track menu | device | **needed** |
| Chapters strip · Info panel | device | **needed** |
| Skip Intro / Skip Credits pill | device | **needed** |
| Who's watching picker | device, purpose-made account | **needed** — see §5.3 for why it is not taken from a real roster |

The needed captures are all reachable on the bench with the boot triggers documented in the root
`CLAUDE.md`; none of them requires new code.

---

## 10. Provenance

- Key codes: LG's evdev→scancode table in the television's own `libSDL2-2.0.so.0.4.1`, plus 336
  captured key lines. Settled 2026-08-22.
- Transport state-mark rule: `rust-modules/src/ui/player_hud.rs::transport_mark`.
- Key dispatch: `rust-modules/src/ui/consts.rs::classify` and `rust-modules/src/app.rs`'s ladder.
- Screen map: the `Route` enum in `rust-modules/src/app.rs` and `ui::trail`.
- Audio: `Makefile` `LIBS_REAL`, and `SDL_Init(SDL_INIT_VIDEO)` in `rust-modules/src/app.rs`.
- Figures: `make sim`, 2026-08-23, at 1920×1080, dev counter compiled out.

**Exactly one figure is edited, and the edit is stated where it appears**: the Search scope line
(§5.5), which named a real server. No other figure has been retouched in any way.
