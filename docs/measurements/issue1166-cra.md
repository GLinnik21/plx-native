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
It is not the reporter's exact library. That affected 5.5/o20 ELF was not found in the
available DAS images or prior local labs. Logs from the two builds therefore cannot be
explained by treating them as identical firmware.

The newer plugin's supported properties cover dual-stream, secure-path and memory
configuration. No public first-IDR bypass was established. Secure-video paths require
secure buffers, and relabeling CRA as IDR would misdescribe the bitstream. Unbounded
decode preroll from an earlier IDR is not a verified or bounded workaround. Obtain the
affected plugin's version/hash and, through a private channel, the exact binary before
claiming a binary-level comparison or a DV-preserving fix.

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
