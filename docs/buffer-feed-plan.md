# Buffer-feed playback plan (Starfish BUFFERSTREAM)

Why we pivoted here: the earlier URI-mode (`com.webos.media/load type:"media"`) experiments ran
the pipeline **out-of-process** in uMS and failed to bind its video plane
(`tv.display/setMediaVideoData` → ERROR_06 in those attempts). That result did not establish
an architectural prohibition: the [2026-11-02 controlled experiment](measurements/uri-acb-dass-2026-11-02.md)
displayed its SDR720p24 fixture through ACB on the webOS 4.5 dev set, both with and without
`connectDass`. The individual contribution of its shared setup changes remains undetermined.
The Kodi/Moonlight approach runs the pipeline **in-process** via
`StarfishMediaAPIs` (libplayerAPIs), so ACB binds the app-owned sink. Audio already works
via ACB in the earlier experiments; the video-plane failure was the reason for that pivot.
The firmware inventories and URI-mode findings are recorded in `docs/uri-mode.md`.

## Components to build

### 1. C++ shim binding `StarfishMediaAPIs` (libplayerAPIs.so.1)
Compile with `zig c++` (same triple), link against a stub `libplayerAPIs.so` carrying the
real SONAME (same pattern as the other stubs). Expose C entry points to main.c.
Real symbols (mangled) on the TV:
- ctor `StarfishMediaAPIs::StarfishMediaAPIs(const char*)` = `_ZN17StarfishMediaAPIsC1EPKc`
- `Load(const char* payload, void(*cb)(int type, int64 num, const char* str))` = `_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_E`
- `Feed(const char*) -> std::string` = `_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc`
- `Play()` `Pause()` `Unload()` `Seek(const char*)`, dtor `D1Ev`
- helpers: `getMediaID()`, `isLoadCompleted()`, `getCurrentPlaytime()`, `setExternalContext(GMainContext*)`
Object size unknown → allocate an over-sized aligned buffer (e.g. 64 KB), construct in place
by calling the ctor symbol on it; never hand it to C++ new/delete. `Feed` returns a cxx11
std::string (read char* at offset 0 for the `"Ok"`/`"BufferFull"` reply).

### 2. Load payload (BUFFERSTREAM, from ss4s `MakeLoadPayload`, per-ES codec info)
`{args:[{mediaTransportType:"BUFFERSTREAM", option:{appId, externalStreamingInfo:{contents:{
codec:{video:"H264"|"H265", audio:"AC3"|"AAC"}, esInfo:{ptsToDecode:0, seperatedPTS:true},
format:"RAW", provider:"..."}, bufferingCtrInfo:{...}}, needAudio:true, ...}}]}`
Codec info comes from the Plex `/library/metadata` Media/Part/Stream fields.

### 3. Feed loop
`Feed('{"bufferAddr":"%p","bufferSize":%u,"pts":%llu,"esData":1|2}')` — esData 1=video, 2=audio;
pointer is in-process. Check reply for `"Ok"` / `"BufferFull"` (backpressure → retry).

### 4. Demuxer — needs raw ES (no container-ingest transport; only BUFFERSTREAM/dual_stream)
Plan: have PMS **remux to MPEG-TS** (H264+AC3 direct-stream copy, cheap), then a minimal
in-app TS demuxer (188-byte packets → PID filter → PES → ES + PTS). ~300–500 lines C.
Plex endpoint: `/video/:/transcode/universal/start.m3u8?protocol=hls...` (HLS, TS segments)
— exact params still TBD (400s so far; needs full X-Plex-* client headers + session).
Fallback: cross-compile libavformat (heavier).

### 5. ACB bind (unchanged, already coded)
setSinkType(MAIN) → setMediaId(getMediaID()) → setState(LOADED) → setDisplayWindow →
on videoInfo: setMediaVideoData (should now succeed — in-process pipeline). setState(PLAYING).

## Validation ladder (each needs one screen look)
1. Shim + Load(BUFFERSTREAM) + ACB + feed a small pre-made ES sample → does video show?
   (de-risks the whole architecture before HTTP/demux). Need an H264+AC3 ES sample source.
2. Add TS demux, feed from a local .ts file.
3. Add HTTP fetch from PMS, full pipeline.

## Open questions
- Exact Plex params for a plain MPEG-TS/HLS stream (current requests 400).
- ES sample for step-1 validation (no ffmpeg locally or on TV yet — check TV for gst-launch).
