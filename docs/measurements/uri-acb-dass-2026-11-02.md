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

## Local evidence artifacts

The initial baseline was stopped before deployment because `tv-session.sh up` attempted an
automatic rebuild under the lease. It supplies no playback result. The application was closed,
automation triggers cleared and the lease released. Subsequent runs must use a committed,
checked binary built before acquiring the television.
