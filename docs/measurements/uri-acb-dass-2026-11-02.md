# URI ACB connection experiment, 2026-11-02

This experiment tests a specific native URI binding sequence on the webOS 4.5 dev set.
It does not enable URI playback for users or settle the webOS 5+ transport questions in
issue #1064. Its base is the URI ownership foundation on `feat/uri-playback`, stacked on
`probe/uri-transport`; neither probe PR is rewritten or landed.

## Source of the hypothesis

[Stremio's native webOS client](https://github.com/stremio-origin/webos/blob/051818e55cf38caed42d597fbcb05bdd3fc5bbd9/src/platform/webos/video_webos.c)
uses `com.webos.media/load`, then `notifyForeground` with `connectionId` equal to the returned
media ID. On ACB firmware it initializes player type 0, sink 0, and sequences media ID,
LOADED, `connectDass(0)`, video metadata, full-screen window, and PLAYING. Delays between
these stages are 200, 200, 200, and 300 ms. Its load carries `window_id_dummy`.
Its [compatibility record](https://github.com/stremio-origin/webos/blob/051818e55cf38caed42d597fbcb05bdd3fc5bbd9/docs/compatibility.md)
reports testing on 4.4; this is a hypothesis for our set, not proof of our binding.

## Firmware evidence before adding the call

Freshly copied from the dev television under `tools/tv-lock.sh`:

- Library: `/usr/lib/libAcbAPI.so.1.0.0`, SONAME `libAcbAPI.so.1`, ARMv7 EABI.
- SHA-256: `d9627a2f7e2c5efe8ab562b94fa6640b93d01da6e6ba41654aef69e22b24af1b`.
- Exported `AcbAPI_connectDass` at ELF virtual address `0x16720` receives the handle,
  integer mode, and `long *taskId`. It preserves r1/r2, resolves the handle, then calls
  `ACB::AcbCore::connectDass(long, long*)` with mode + 1 and the same task pointer.
- The core at `0x10a14` creates task type `0x0d` using that pointer and dispatches an
  asynchronous LS2 request. Its return value is dispatch evidence, not completion evidence.
- The request targets `luna://com.webos.service.acb/connectDass` with `appId`, `pipelineId`
  and `dassSink`. The mode lookup uses `ACB::sinkTypeList_`: external 0 becomes internal
  index 1, whose string is `main` (index 0 is `auto`). This follows the GOT relocation at
  `0x2c944` to the table at `0x2c9a8`, rather than inferring a sink from the function name.
- Only fixed mode 0 is considered here. The wrapper contains no range check before the
  core indexes its mode table; arbitrary mode input must not be exposed.
- `AcbAPI_initialize` at `0x15c4c` forwards the core's return value. The core at `0xcd5c`
  returns 1 after its first valid initialization and callback installation; invalid input
  or repeated initialization returns 0. A nonzero create handle alone is not this evidence.
- Offline inventories contain the export on 3.4.0, 3.9.2, 4.4.2 and 4.10.0. It is absent
  on 2.2.3; the entire library is absent on 5.3.1 and later inventoried firmware.
  The call must remain optional and cannot become a requirement for ordinary ACB playback.

Ghidra's analysis image is rebased by `0x10000`: its wrapper/core addresses are
`0x26720`/`0x20a14`. These are the same functions, not another firmware build.

## Controlled comparison

Both arms use a fresh application process and the same synthetic H.264/AAC Matroska file:
1280 × 720, 24 fps, 180.021 seconds, moving colour bars. No household media or account token
is involved. The mock's synthetic identity must be checked before playback. Sound and panel
are off; service captures include the hardware video plane.
Fixture SHA-256: `a037332fb1b3e291ea59219b0ea99ab4cf720abd797311b44a57ae2fccf116fc`.

The arms must match in player type, sink, dummy window ID, foreground notification, media,
descriptor, stage timing and play dispatch. The control waits through the connection stage
without making the call; the other arm calls `connectDass(0)`. Missing actual dimensions or
frame rate, a failed prerequisite, or an unavailable connection entry point makes an arm
inconclusive. API acknowledgements and advancing `currentTime` alone cannot show that the
picture bound. Compare at least two hardware-plane captures and check the stopped state.

Service Play must not wait for the ACB descriptor. The pinned client's load reply sends Play
after subscribing, before its later binding stages. This probe preserves its existing Play at
`loadCompleted`, independently of the ACB sequence, in both arms. It must accept video facts
that arrive only after Play, and must not send a second Play when ACB reaches PLAYING.

URI bus unregister and asynchronous ACB completion are separate facts. After acknowledged
URI release the experiment sends ACB UNLOADED once and retains its slot until application
exit; the next arm starts a new process. This avoids asserting an unproven ACB release fence.

## Device result

**Both arms displayed moving URI video on the webOS 4.5 dev set.** The control did not invoke
`connectDass`, so that call is not necessary for the demonstrated sequence. The individual
contribution of the shared changes (foreground notification, player type, descriptor, dummy ID,
timing and full-screen binding) was not isolated. This does not establish all 4.x firmware,
formats, partial rectangles or normal playback support.

Both runs used committed source `01b23f455`, the same debug-flavour release-profile ARM binary,
and fresh application processes. The launcher verified deployed bytes against the local build
before each run. App SHA-256:
`edf5e7b7f86798640a93aa119d92000264e1f762dddbc7a85b04b29d5c82af30`.

| Observation | `acb-control` | `acb-dass` |
|---|---|---|
| Native initialization | player type 0, return 1, callback installed | Same |
| URI registration | Named | Named |
| Foreground notification | Accepted | Accepted |
| Video metadata | 1280 × 720, 24 fps, bitrate 4,139,352 | Same |
| Descriptor | Explicit SDR720p24 fixture | Same |
| `connectDass(0)` | Skipped, timed slot retained | Return 1, task 2, callback observed |
| Display captures, embedded media time | 13.750 s and 21.375 s | 12.792 s and 20.125 s |
| Video-only capture, embedded media time | 24.625 s | 23.375 s |
| Picture | Moving colour bars in all captures | Moving colour bars in all captures |
| Back | URI unregister, one ACB UNLOADED, retained quarantine | Same |
| Stopped display | Home; no fixture picture visible | Same |

All captures were opened and inspected. These are service captures with the panel off, not GL
readbacks or photographs of the physical panel. Sound was muted and audibility was not tested.

The call completion → next-dispatch intervals were 208/216/215/312 ms in the control and
209/216/215/315 ms in the Dass arm, meeting the same requested 200/200/200/300 ms spacing.
All native stage dispatches returned 1; the callbacks recorded no recognized `ERROR_06`.
Those observations supplement the pictures; neither a dispatch result nor absence of an error
would establish visible binding by itself.

Both stops reported successful bus release, sent UNLOADED exactly once, observed its callback
with play state 0, and retained the deliberate `need-relaunch` quarantine. The stopped display
showed Home. This is not a general asynchronous ACB release proof, and the ownership fence remains.
The driver closed each app, removed automation triggers, stopped its reverse mock tunnel and
released the TV lease after the run. No household account was used.

Pause/resume, seeking, duration reporting, subtitles, EOS and sustained playback were not tested
by these short picture-binding arms. In particular, this does not replace the webOS 5.4.2/26
transport logs requested through #1064 or enable a production URI route.

### Captures and logs

The images are unchanged service output. The logs retain only URI probe and numeric ACB evidence;
raw application/session logs are not published. [Artifact hashes](uri-acb-dass-2026-11-02/SHA256SUMS)
pin the exact saved bytes.

| Arm | Display 1 | Display 2 | Video only | Stopped | Log |
|---|---|---|---|---|---|
| Control | [13.750 s](uri-acb-dass-2026-11-02/control-frame1.png) | [21.375 s](uri-acb-dass-2026-11-02/control-frame2.png) | [24.625 s](uri-acb-dass-2026-11-02/control-video.png) | [Home](uri-acb-dass-2026-11-02/control-stopped.png) | [Control](uri-acb-dass-2026-11-02/control.txt) |
| Dass | [12.792 s](uri-acb-dass-2026-11-02/dass-frame1.png) | [20.125 s](uri-acb-dass-2026-11-02/dass-frame2.png) | [23.375 s](uri-acb-dass-2026-11-02/dass-video.png) | [Home](uri-acb-dass-2026-11-02/dass-stopped.png) | [Dass](uri-acb-dass-2026-11-02/dass.txt) |

## Host verification

On committed source `01b23f455`, with a clean tree before and after the gates:

- `make check` passed both Rust suites, all 119 Python steps and all 546 mock harness tests.
- `make check-nodef` and `make FLAVOR=debug ARM_PROFILE=release` passed.
- ELF instruction/dependency checks passed. The private-IP/default-placeholder checks were
  skipped because the build used an ignored synthetic mock configuration.
- The app passed all nine gated firmware import inventories; the helper passed all 14.
  Both linker-input/SHA-256 receipts matched their binaries.
- PR, firmware-compatibility and prose reviews completed. Four additional regressions failed
  before the media-identity and native-initialization review fixes, then passed.

The final media hostsim result was:

```text
test result: ok. 2276 passed; 0 failed; 78 ignored; 0 measured; 0 filtered out; finished in 90.09s
```

The experiment remains Sans-IO: the machine receives time and addressed answers and emits typed
effects; the driver executes bus and native calls. The dependency gate reports unchanged
worker/global/fork/effect-lane ledgers.
