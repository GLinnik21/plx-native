# Issue 1166: Dolby Vision random-access gate

## Observed on the affected television

The reporter's [2026-11-03 reply and attached traces](https://github.com/GLinnik21/plx-native/issues/1166#issuecomment-6604709736)
identify webOS 5.5.0-19 on o20. The commercial file cannot be shared; the public samples
they tried did not reproduce the failure. A manual seek reproduced the hang within the
diagnostic recorder's time window.

The working start in `seek-firmware-trace.log` finds an IDR of HEVC NAL type 20. The
splitter then pushes video, the metadata parser receives data, and the dual sequencer
forms video/metadata pairs. After the flush, the first random-access unit is CRA type 21.
The splitter logs `non IDR. skip this buffer` and never pushes another video buffer in
that capture. RPU NAL type 62 is present in the discarded units. The resume capture has
the same rejection and no downstream video pushes or pairs.

| Capture/window | CRA NALs inspected | Non-IDR skip log lines | Splitter video pushes | Pair log lines |
|---|---:|---:|---:|---:|
| Resume | 1 | 84 | 0 | 0 |
| Seek, after the new segment at 12930 ms | 80 | 16926 | 0 | 0 |

The two skip log sites produce two messages per discarded buffer. These counts locate
the observed blockage upstream of decoding and metadata pairing; they do not establish
a safe application-side workaround. HEVC NAL identities follow the
[FFmpeg enum](https://ffmpeg.org/doxygen/8.0/libavcodec_2hevc_2hevc_8h.html).

## Firmware boundary

The archived DAS W20O firmware 04.64.00 is webOS 5.6.2. Its
`libgstdvsplitter.so` SHA-256 is
`e03029b263ef6eb453cd2df4b4200fba3b14326ca113cdcf2ecf06b2f449457a`.
Its initial HEVC random-access scan accepts NAL types 16 through 21, including CRA.
It is not the reporter's exact library. The affected 5.5/o20 ELF (5.5.0-19, `jhericurl-jervisbay`,
SHA-256 prefix `0cda1717`) is archived (`W20O@04.50.90`); no other archived 5.5.0-19 image
carries a splitter (`W20P` has none). Logs from the two builds (5.5.0-19 and 5.6.2) therefore
cannot be explained by treating them as identical firmware.

The NAL-type dispatch of the other archived splitters was read from the images, next to the
5.5.0-19 one (addresses and conclusions only):

| Firmware | Splitter SHA-256 prefix | Dispatch | NAL types that set the first-IDR latch | CRA first |
|---|---|---|---|---|
| 4.10.2-31 (`goldilocks2-grampians`, W19H and W19O, the dev set's family) | `b38aedc9` (W19H), `4096b724` (W19O); the code is the same in both | `tbh` at 0x4c98, 16 to 21 all reach 0x50a4 | 16 to 21 (latch at +0xe4) | accepted |
| 5.5.0-19 (`jhericurl-jervisbay`, W20O) | `0cda1717` | byte table at 0x5b12 | 19 and 20 only; 16, 17, 18 and 21 bump a counter | skipped |
| 5.6.2-21 (`jhericurl-jimna`, W20O) | `e03029b2` | initial scan | 16 to 21 | accepted |
| 6.5.3-47 (`kisscurl-koli`, W21O, the LG C1) | `239b3e04` | `tbb` at 0x5b16, table base 0x5b1a | 19 and 20 only (branch at 0x5b6a, latch at +0xe0 stored at 0x5baa); 16, 17, 18 and 21 go to the counter-only branch | skipped |

So the dev set (4.10.2) accepts a CRA first and cannot reproduce #1166, and the C1 row
(`devcaps::EXCLUSIONS`, `kisscurl-koli`) rests on this reading alone: it is a disassembly of the
6.5.3-47 image's splitter, and the hang was not observed on a C1.

The newer plugin's supported properties cover dual-stream, secure-path and memory
configuration. No public first-IDR bypass was established. Secure-video paths require
secure buffers, and relabeling CRA as IDR would misdescribe the bitstream. Unbounded
decode preroll from an earlier IDR is not a verified or bounded workaround. Still open: the
reporter's own TV was never hashed (its firmware string matches the archived 5.5.0-19 image, which
is not the same as the library being byte-identical), and a fix that KEEPS Dolby Vision at a CRA
landing is unestablished. The shipped answer withdraws the declaration and plays the HDR10 base
layer.

## Original test material and diagnostic export

[The shared fixture generator](../../tests/fixtures/README.md) creates a DV CRA clip and
an IDR-only control without donor media. Read-back verification distinguishes their
actual random-access NALs and checks RPU presence. Reproduction on the affected TV is
still required; a successful run on another firmware does not settle that question.

The reporter also could not find the combined saved log. The original app events record
capture starts/stops but no export stage or error. That evidence does not distinguish
failure to dispatch, worker refusal, file validation, copy failure, or looking in another
directory. The follow-up export changes add fixed-label stage/OS-error reporting and
retire Saving after a refused/disconnected worker. They do not claim to have identified
the reporter's filesystem failure.
