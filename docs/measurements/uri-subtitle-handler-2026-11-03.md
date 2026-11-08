# URI subtitle-enable handler evidence — 2026-11-03

This inspection concerns the W19H firmware image `HE_DTV_W19H_AFADABAA` and its cached
`libplayerAPIs.so.1.0.0`, SHA-256
`5b5252c5984764760e29977ff12bc92125ae091b55de4dc56d2870cd6f67a4b3`.
The binary was inspected locally with the repository's `decompile-tv-lib` workflow. No device
state or household account was changed.

## Observed native path

At `0x00058b54`, `StarfishMediaPipe::setSubtitleEnableEvent` reads the incoming message,
calls `StarfishMediaAPIs::setSubtitleEnable`, then passes that call's result to `SendResponse`.
The inspected native handler therefore responds after invoking the setter.

At `0x0004206c`, the setter parses the message through `FUN_00041edc`. On a successful parse it
updates subtitle-task visibility; one branch also resets subtitle state. It may call an
additional pipeline virtual method. It returns the parser result. The helper's field validation,
the virtual method's completion semantics, and the visibility argument's internal meaning have
not been established by this inspection.

The cached `umediaserver` query found a `PipelineManager::setSubtitleEnable` forwarding stub at
`0x00027adc`. That stub does not establish when the public `com.webos.media` method responds.

## Scope of the evidence

These findings establish neither asynchronous completion nor the public service response's
ordering relative to this native handler. They do not establish selected-track notification,
rendered output, reusable display release, or behavior on other firmware images.

The separate synthetic 4.5 fixture recorded successful subtitle-enable/disable responses and
captured an embedded SubRip caption appearing and disappearing. That fixture supports those
specific observations; it cannot turn every method acknowledgment into track-state or picture
evidence. Product URI capabilities must continue to require explicit qualification and an
addressed confirmation path. This inspection alone does not enable `subtitle_off`.
