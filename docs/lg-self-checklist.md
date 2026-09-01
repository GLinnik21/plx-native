# LG App Self Checklist — measured state, item by item

**The checklist is a submission document, not an internal confidence score.** LG names it as a
required webOS TV submission document, and its own preamble requires every item to be marked Pass or
N/A before submission. An unrun item cannot honestly be marked Pass; a Fail must be debugged before
submission.

Checklist version 5.0 (updated 2022-09-27), 53 items. Status taken **2026-08-24** against the
integrated release candidate documented by this commit.

Native eligibility is no longer the open question this file used to describe. Seller Lounge accepts
this IPK as **File Type: Native** and asks for its native SDK version, chipset and resolution; see
`docs/distribution.md` §2. The remaining questions are QA and submission-readiness questions.

## 1. Integrated-plan completion is not LG submission readiness

The Claude integration plan is complete in this tree: T2/T4/T3/T3b and PR #60 are present. Its
measured gates are:

- host `make check`: **1215/1215** tests passed;
- harness unit suite: **62/62** passed;
- `--no-default-features`: green;
- ARM cross-build: green;
- firmware compatibility: **OK for webOS 4.4.2 through 11.2.0**;
- exact television/debug build: synthetic **21/21**, server **21/21**, and `--fps-player`
  **16/16**.

Those results prove a great deal about this exact integrated implementation. They do **not** turn an
unrun LG item into Pass, do not substitute for Store-distribution evidence, and do not resolve a
submission-policy conflict. The exact television run was the debug build; the release configuration
was type-checked and cross-built, not silently treated as the same artefact.

## 2. Remaining submission blockers — NOT DONE

### #53 — factory reset, then execute the app: EXTERNAL EVIDENCE UNAVAILABLE

Native Store submission is possible, but this item cannot be answered with a Developer Mode or
Homebrew sideload: a factory reset removes that installation route. It needs an actual
Store-distributed build, followed by factory reset, install/restore through the supported
distribution channel, and launch. That distribution evidence is not available yet, so this row is
Open rather than N/A.

### Root BACK on the entry page: POLICY CONFLICT

This is a submission-policy blocker in addition to the numbered checklist. The current app raises
its Cancel/Exit confirmation at Home root. The webOS 23–25 submission expectation says entry-page
BACK must show the television Home screen. Closing it requires either native-specific Seller Lounge
or Native SDK evidence that native apps are exempt, or a code change followed by device
verification. The existing exit alert is not evidence of compliance.

## 3. Device evidence still unknown — do not mark Pass

| # | Item | Evidence still required |
| --- | --- | --- |
| 3 | Reboot / lifecycle | Run the native **16-case lifecycle matrix**: remote power off/on, AC unplug/replug, Recent List close/relaunch, and both launch paths. Verify every cold relaunch settles on the credential-selected Home route (never an old Detail/Library page), while an app switch with the process still alive preserves the separate suspended-playback resume path. `handlesRelaunch: false` and `nativeLifeCycleInterfaceVersion: 2` do not answer native behaviour by themselves. |
| 13 | LockUp / LatchUp | Run the exact **4-hour continuous soak**: `plxnative-homeosc` grid sweep for 1 h, full-length playback for 2 h, then `plxnative-navosc` route bounce for 1 h. Check `fuser` liveness and the crash log at every hand-off. Time accumulated during unrelated testing is not this evidence. |
| 14 | Abnormal end | Run wired and wireless, each with static and dynamic IP, while applying the reproducible `rate:` and `blackhole` failure modes. The proxy can create the condition; it cannot reconfigure the television's network. |
| 15 | Keyboard cursor/editing | On the LG VKB, verify cursor movement and editing with `<` and `>` rather than inferring SDL event delivery from the host simulator. |
| 16 | Keyboard character fidelity | Type the checklist's literal `\` example and verify case transfer on the television. |
| 17 | Keyboard linked buttons | Exercise Voice Search and the LG VKB's other linked buttons on the television. |
| 20 / 43 | Remote server reachability | With the LAN route blocked, sign in, browse and play from the account's remote server. Record the winning HTTPS `plex.direct` origin in the event log. HTTPS control, TLS media, relay fallback and persisted per-server state are implemented, but this exact acceptance run is still owed. |
| 43 CASE2 | IPv6 | Browse and play over a **v6-only route**. `AF_UNSPEC`, IPv6 URL bracketing and both address families are implemented and host-tested; that is not an on-device IPv6 playback result. |
| 22 CASE2 | Unsupported-language search | Enter a CJK query and visually verify Han/Kana/Hangul text rather than tofu. The bundled fallback font and per-run fallback renderer are host-gated; RTL remains explicitly outside that claim. |
| 26 | General IR remote | Capture every physical button on a standard IR remote, slowly and in recorded order, and decode the raw scancode/symbol pairs. The Magic Remote capture is complete; the IR remote is not. |
| 36 / 39 | HOME and LIVE keys | Include HOME and LIVE in that capture and record what the platform actually delivers or consumes. They are intentionally not guessed or bound from web keycodes. |
| 40 | Unsupported keys | Sweep unsupported keys on normal routes and during playback; verify they move no focus, wake no player HUD, and cancel no armed press. Host invariants do not observe the real remote or video HUD. |

### Unnumbered release evidence still owed

These are not converted into numbered checklist failures, but they remain explicit release risks:

- cancel an in-flight DNS lookup and an in-flight TLS handshake on the television, proving prompt
  teardown with no stranded worker;
- resolve and play a hostname through the app's glibc NSS path. LG libcurl's c-ares result does not
  prove glibc NSS works inside the native jail;
- measure the integrated build's worst-path `VmHWM` through playback with the CJK fallback exercised
  and confirm it remains below `requiredMemory: 160`. The earlier approximately 152 MiB peak predates
  the fully exercised bundled CJK face and is not sufficient evidence for this build;
- install the packaged IPK — **install, not deploy** — switch the set to Korean, and verify the
  launcher/listing keeps `PlxNative debug` while using the Korean description. Re-check after reboot
  for SAM caching and try the documented region-qualified resource layouts if bare `ko` is ignored.

## 4. Newly device-passing on 2026-08-24

| # | Item | Measured basis |
| --- | --- | --- |
| 43 CASE1 | Adaptive resolution under four shaped links | **Pass on this television.** Auto starts from a conservative 720 Kbps/480p request when nothing about the link is knowable for free, measures segment throughput, PMS production time and normalized A/V buffer time, and primes a separately named fixed-rendition encoder before switching. `tools/netcond.py` produced the required results: 512 Kbps → 320 Kbps / actual 320×134; 1 Mbps → 720 Kbps / actual 480×200; 7 Mbps → 4 Mbps / actual 1280×536; 17.5 Mbps → 8 Mbps / actual 1920×804. A live high-to-512 Kbps collapse jumped directly to the sustainable floor rather than downloading oversized intermediate rungs. All switches stayed inside one Starfish Load, the full 1:40:03 duration remained stable, and a mid-movie seek resumed on the correct HLS segment. **Those four settle values were measured on the six-rung ladder of that day**; the controller was rewritten on 2026-08-25 onto a 13-point catalog, where the 17.5 Mbit/s leg would land on the 10 Mbps rung instead of 8. That is the one number here the rewrite is expected to change, and it has not been re-measured on a device — the checklist item is about adapting to the shaped link, which each leg still demonstrates. |
| 46 | Replay after completion | **Pass on this television.** The synthetic suite ran `pipe_finish_eos` and `pipe_replay_after_eos`: the 20 s clip reached `EOS reached → ended`, tore down, re-entered exactly once, produced a second Load and fixture fetch, and its media position fell and climbed again. This proves trigger-driven direct-play replay; user-driven and transcode replay remain useful extra coverage, not a reason to erase this measured result. |
| 50 / 51 | Resolution × codec | **Pass on this television.** All eight SD 720×480 / HD 1280×720 / FHD 1920×1080 / UHD 3840×2160 × {H.264, HEVC} cells passed, each grading exact `expect.video_size`. The 4096-wide boundary, refusal above it, and a real PMS decision for generated-only shapes remain outside this matrix. |

The complete debug-device evidence behind those rows is broader than the two checklist closures:

- synthetic **21/21** proves transport, demux, feed, codec declarations, EOS/replay, seek, frame-rate
  fixtures, and the exact resolution matrix;
- server **21/21** proves the live Plex selection path, direct play/transcode decisions, PlayQueue,
  track selection, markers, resume and timeline reporting for the configured library;
- `--fps-player` **16/16** proves every configured UI and player performance scene met its gate on
  this television.

Synthetic and server tiers are complements. A future run with skipped server shapes must report the
skips; a partial green count is not equivalent to 21/21.

## 5. Other passing items and their evidence

| # | Item | Basis |
| --- | --- | --- |
| 1, 2 | Execution, main screen | Repeated device-suite launches reached the intended route. The 1920×1080 UI is held inside the 5% overscan frame by `MARGIN_X` 96 / `MARGIN_Y` 54 and the composed-geometry host gate. The splash is 1920×1080 and uses the app surface rather than a black field. |
| 6 | Correct text | `text::elide`, long-form scrolling, synthesized music-note glyphs, and the bundled CJK fallback cover the implemented scripts. The separate CJK visual acceptance run in §3 is still owed; this row does not claim RTL support. |
| 7 | Focus / mouse-over | Idle, focused and selected states are distinct and were audited screen by screen at 1920×1080; the 16/16 FPS run exercised the integrated focus/transition scenes. |
| 8 | Flickering | No flicker was observed in the completed device matrices; the known Dolby Vision Profile 5 pulse is fixed. |
| 9 | Full-size video | Video track and plane are full-panel 1920×1080 with no application margin. |
| 21 | Sign out | `ui/account_menu.rs` reaches `auth::sign_out`; session removal is host-gated. |
| 27–31, 33 | Magic Remote, pointer, OK, wheel, navigation | Pointer/click and wheel paths are device-proven; the Magic Remote raw capture contains 336 real key lines. |
| 37 | BACK within the app | BACK returns through the route trail. Home-root behaviour is the separate policy blocker in §2 and is not hidden by this Pass. |
| 38 | EXIT key | Scancode 505 is bound and device-verified: press → `EXIT key: terminating` → no process. |
| 45 (remote half) | Playback control keys | Native scancodes for play/pause, stop, rewind and fast-forward are bound and device-proven. The absence of an on-screen transport row is documented in the submitted UX scenario rather than disguised as an implementation gap. |
| 48 | Subtitles | The server matrix passed text and image subtitle paths; music-note coverage is bundled. Subtitle-appearance settings are N/A because the app exposes none. |
| 49 | Resume | The server matrix passed the PMS-backed `viewOffset` resume path; the harness resets state through `/:/unscrobble`. |

## 6. Decisions and N/A items

**#41 — language setting.** N/A on the item's own precondition because the app exposes no in-app
language selector. The implemented halves are still useful: localized `appinfo.json` resources cover
launcher/listing metadata, and a validated inherited POSIX locale becomes `X-Plex-Language` for PMS
metadata. Their Korean television acceptance recipe remains open in §3; implementation is not being
misreported as observation.

**#45 — on-screen transport controls.** The product deliberately uses a remote-driven, state-only
transport HUD rather than a focusable Play/Pause/Stop/FF/RW row. The compliance description belongs
in `docs/ux-scenario.md`, which is what the checklist tells QA to consult for movement details.

Genuinely N/A: #4 advertisement · #11 BACK UI button · #12 EXIT UI button · #18 terms · #19 sign-up
(account creation happens on plex.tv) · #23 adult authentication · #24 / #25 payment · #32 colour
keys (unused) · #35 MMRC-only restriction · #42 in-app UI sound control (there is no UI audio) · #44
full/original screen toggle · #47 live/real-time TV streaming · #48 subtitle appearance settings ·
#52 DRM.

## 7. Release verdict at this snapshot

The integrated implementation and its automated/device suites are green. The LG submission is
**not ready**: Store factory-reset evidence is unavailable, root BACK is in policy conflict, and the
unrun device evidence in §3 cannot honestly be marked Pass. Automatic ABR and LG #43 CASE1 are no
longer blockers. Keep plan completion and submission readiness separate when reporting status.
