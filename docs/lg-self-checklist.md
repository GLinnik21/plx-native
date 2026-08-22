# LG App Self Checklist — where this app stands, item by item

**The checklist is a submission DOCUMENT, not a rubric you grade yourself against privately.** LG
names it as *the* required document for a webOS TV submission (NetCast used a "Technical Note"
instead), and its own preamble sets the marking rule:

> All test items must have either a "Pass" or "N/A" result before submitting. If an item is marked
> as "Pass" when its result is supposed to be "N/A" and/or if an item is marked as "N/A" when it is
> supposed to be "Pass", the app can be rejected for not providing accurate information. In case
> there is an item marked as "Fail", the app must be submitted post debugging.

So **"we never tested it" is not a markable state.** That is the real reason the untested column
below matters as much as the failing one: an honest submission cannot mark those Pass, and marking
them Pass anyway is itself a documented rejection ground.

Checklist version 5.0 (updated 2022-09-27), 53 items. This file records **our** status against it.
Read `docs/distribution.md` §2 first for the eligibility question this document deliberately sets
aside — LG's live `appinfo.json` reference still says of `type` that *"Only `web` is allowed
currently"*, and whether a native ipk is submittable at all is unsettled.

Status taken 2026-08-22.

---

## 1. The four that FAIL, and why

### #43 CASE1 — resolution must change with network speed

There is no adaptive bitrate anywhere. `route.rs` makes one direct-play-vs-transcode decision at
start and holds it for the session. The item's own legs are 512 Kbps → 1 Mbps → 7 Mbps →
17.5 Mbps with the remark *"buffering should not occur constantly"*; the two slow legs would buffer
continuously.

### #43 CASE2 — IPv6

`stream.rs` is `AF_INET` only (lines 284, 307, 703, 716, 731, 788, 867). Playback over IPv6 is not
degraded, it is impossible.

### #20 / #43 / #47 — a tester cannot reach a server at all

**This is the one that fails first in a lab, and it fails three items at once.**
`stream.rs::http_open` builds a `sockaddr_in` by hand from four decimal octets: plaintext HTTP, no
TLS, no name resolution. `auth.rs` ranks every candidate the account can reach, but the transport
still cannot dial an `https` `plex.direct` origin, a hostname, IPv6, or a relay connection — and
there is no Settings screen and no manual server entry anywhere in the route enum. Unless the QA
bench sits on the same IPv4 LAN as a plaintext-HTTP PMS, the app dead-ends at "no server found".

### #53 — factory reset → execute app

Undefined for a sideloaded app: a factory reset removes Developer Mode and the Homebrew Channel
along with it. Meaningless in our distribution model, unanswerable in LG's.

---

## 2. The items nobody has run

Not failures — *unknowns*, and unmarkable. Every one of these is a device session, mostly not code.

| # | Item | What is actually unknown |
| --- | --- | --- |
| 3 | Reboot | The whole matrix: remote power off/on, AC unplug/replug, Recent List behaviour, playback resumed after reboot. `handlesRelaunch: false` is set and its consequences are untested. |
| 14 | Abnormal end | Wired vs wireless × static vs dynamic IP. |
| 16 | Keyboard character fidelity | The item's own example — does `\` arrive as typed? |
| 17 | Keyboard linked buttons | The LG VKB's Voice Search and its siblings. |
| 26 | General (IR) remote | Every key. Everything here has only ever been driven with the Magic Remote — see §4, which is why this one is not a formality. |
| 36 / 39 | HOME and LIVE keys | Assumed system-handled; never observed. HOME is scancode 269, unbound. |
| 40 | Unsupported keys | `Key::Other` swallows them so it is *probably* safe, but unproven. |
| 46 | Replay after completion | Untested. |
| 50 / 51 | Resolution × codec | Pieces are covered by `tests/run.py`; never run as the matrix the item asks for. 4K HEVC 10-bit HDR10 was device-verified 2026-08-22, SD/HD were not. |
| 13 | LockUp / LatchUp | No known instance; never formally exercised. |
| 22 | Search CASE2 | A query in a language the app does not support. |

---

## 3. Passing, with the evidence

| # | Item | Basis |
| --- | --- | --- |
| 1, 2 | Execution, main screen | Launches and reaches Home; UI authored at 1920×1080 with `MARGIN_X` 90 (4.7%), inside the 5% overscan frame. Splash is 1920×1080 PNG. |
| 6 | Correct text | `text::elide` for overflow, `ui/text_view.rs` for long-form scroll. **Was failing until 2026-08-22** — see §4. |
| 7 | Focus / mouse-over | Idle, focused and pressed states are distinct (focus scale spring, glow, `ui/press.rs`). |
| 8 | Flickering | No known flicker; the Dolby Vision Profile 5 pulse is fixed (`docs/dolby-vision.md` §4). |
| 9 | Full-size video | Video track and video plane are both full-panel 1920×1080; no margins. |
| 21 | Sign out | `ui/account_menu.rs` → `auth::sign_out`. |
| 27–31, 33 | Magic Remote, pointer, OK, wheel, navigation | Pointer and click path device-proven; wheel at `app.rs:4794`. |
| 37 | BACK key | Returns through the route trail; at Home's root raises `ui/exit_alert.rs` rather than quitting silently. |
| 38 | EXIT key | **Bound 2026-08-22** to scancode 505 and device-verified: press → `EXIT key: terminating` → `fuser` reports NONE. |
| 45 (remote half) | Playback control keys | **Bound 2026-08-22.** See §4. |
| 48 | Subtitles | Text and image (PGS/VobSub) both render. **Was failing until 2026-08-22** — see §4. |
| 49 | Resume | Server-side `viewOffset`; the harness resets it per case via `/:/unscrobble`. |

---

## 4. What changed on 2026-08-22, and what it teaches

Four items moved, and every one of them moved because somebody **looked at the panel** rather than
at the code. That is the transferable lesson: none of these was visible in a green test run.

**#48 and #6 — subtitles rendered `.notdef` tofu.** A capture of the Family Guy theme showed
`▯NO GLYPH▯ It seems today / That all you see ▯NO GLYPH▯`. Subtitle convention wraps a SUNG line in
a music note, and Inter has none of U+2669–U+266C (2849 codepoints, none of them these). Neither
does the television: `/usr/share/fonts/DroidSans.ttf` has 911 and none either, so no fallback chain
to a system face fixed it. `tools/cut-inter.py` now synthesizes the four glyphs.

**#45 — the transport keys were bound to the wrong namespace.** `WCODE_DPAD_LEFT`/`_RIGHT` =
412/417 were never the D-pad and had never fired. 412/413/415/417 are the CEA-2014-A / LG **web**
keyCodes; the app receives native scancodes. Settled twice over — LG's own evdev→scancode table at
offset `0x92840` of the TV's `libSDL2-2.0.so.0.4.1`, and 336 real key lines off the remote, which
show the D-pad at 80/79/81/82 and no 412/417 at any point. Fast-forward (451), rewind (452),
play/pause (261), exit (505), the real stop codes (120/260) and the real channel rocker (300/301)
are now bound.

**#2 — the splash was 63% black**, against LG's *"The splash screen should not be black."*
`mkicons.py --splash-lift=` raises the black point; cut with the app's own `theme::SURFACE_APP`,
which also makes the splash match the first frame the app draws.

**Not a numbered item, but it was wrong:** `requiredMemory` was 60 against a measured 152 MiB peak,
and webOS substitutes a default of **120** when the field is absent — so 60 asked for less headroom
than declaring nothing. Now 160. `docs/distribution.md` §6.10 has the measurements.

---

## 5. The two items that are decisions, not defects

**#45 — there is no on-screen transport button row, and there will not be one.** The item reads as
demanding Play / Pause / Stop / FF / RW / Previous / Next as focusable UI buttons. This app is
Apple-TV-idiom: transport is driven from the remote, and the HUD shows STATE — a small mark beside
the elapsed clock, rewind / pause / fast-forward / a two-second play mark, and nothing at all while
playing steadily. The owner's decision, recorded because a future reader of the checklist will
otherwise try to "fix" it.

**The compliance route is the UX scenario document, not code.** Item 45's own remark is *"refer to
UX scenario for movement details"*, so the UX scenario submitted to Seller Lounge is what QA grades
the transport against. That makes writing it carefully load-bearing rather than a formality — and
it is the same escape hatch for #10, #42 and #2, which all cite it.

**#41 — the app is English-only.** N/A on the item's own precondition (no in-app language setting),
but a Korean tester on a Korean set still sees an all-English UI. LG supports locale-specific
`appinfo.json` files carrying a translated `title` and `appDescription`, which localizes the store
listing and launcher tile without touching the UI. That is the cheap half and it is not done.

---

## 6. Genuinely N/A

#4 advertisement · #11 BACK *UI button* (webOS: optional; we have none) · #12 EXIT *UI button*
(optional; the KEY is bound) · #18 terms · #19 sign-up (account creation happens on plex.tv) ·
#23 adult authentication · #24 / #25 payment · #32 colour keys (unused) · #35 MMRC-only
restriction (we are not MMRC-only) · #44 full/original screen toggle (no such control) · #47
real-time streaming (no live TV) · #52 DRM.

Two worth stating carefully, because they are the easiest to mis-mark: **#42 sound** — there is no
UI audio at all (no `SDL_mixer`, no BGM, no effects), so only video audio is in scope and there is
no in-app sound on/off; and **#48's second half** — subtitle appearance settings are N/A because we
expose none, and our client-rendered subtitles also ignore the TV's own subtitle settings.

---

## 7. The defect class behind two of the fixes

The ♪ bug is the visible tip of something larger and it is worth recording as its own risk:
**any codepoint outside Inter's coverage tofus identically.** CJK, Hebrew, Arabic and Thai titles
and subtitles all would, and a Plex library with foreign-language content is not exotic. The
television ships `DroidSansFallback.ttf` precisely for this. A per-glyph fallback chain in
`text.rs` is real work, but it is the same defect, it hits the same two items (#6, #48), and
nothing in the host suite can see it.
