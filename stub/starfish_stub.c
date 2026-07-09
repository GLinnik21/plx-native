/* Link-time stub for LG's libplayerAPIs (StarfishMediaAPIs C++ class).
 * Real impl lives on the TV; we only need the symbols to resolve at link.
 * The C++ methods are called from C via their mangled names (see main.c).
 * Bodies are empty — never executed on the host. */
void _ZN17StarfishMediaAPIsC1EPKc(void) {}          /* ctor(const char*) */
void _ZN17StarfishMediaAPIsD1Ev(void) {}            /* dtor */
void _ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_E(void) {} /* Load(payload, cb) */
void _ZN17StarfishMediaAPIs4FeedB5cxx11EPKc(void) {}   /* Feed -> std::string */
void _ZN17StarfishMediaAPIs4PlayEv(void) {}
void _ZN17StarfishMediaAPIs5PauseEv(void) {}
void _ZN17StarfishMediaAPIs6UnloadEv(void) {}
void _ZN17StarfishMediaAPIs10getMediaIDEv(void) {}  /* getMediaID -> std::string */
void _ZN17StarfishMediaAPIs15isLoadCompletedEv(void) {}
void _ZN17StarfishMediaAPIs18setExternalContextEP13_GMainContext(void) {}
void _ZN17StarfishMediaAPIs16notifyForegroundEv(void) {}
long long _ZN17StarfishMediaAPIs18getCurrentPlaytimeEv(void) { return 0; }
void _ZN17StarfishMediaAPIs18setCurrentPlaytimeEx(void) {}
void _ZN17StarfishMediaAPIs4SeekEPKc(void) {}
void _ZN17StarfishMediaAPIs5flushEv(void) {}
void _ZN17StarfishMediaAPIs7pushEOSEv(void) {}      /* pushEOS (Kodi EOF drain) */
void _ZN17StarfishMediaAPIs11SetPlayRateEPKc(void) {}
/* Kodi in-place seek: setTimeToDecode (libplayerAPIs) + CustomPipeline::sendSegmentEvent
 * (real body in libpf-1.0.so.1, already in scope via libplayerAPIs' DT_NEEDED). */
void _ZN17StarfishMediaAPIs15setTimeToDecodeEPKc(void) {}
void _ZN13mediapipeline14CustomPipeline16sendSegmentEventEv(void) {}
/* webOS<11 in-place-seek fallback (CustomPipeline, real body in libpf-1.0.so.1):
 * loadSpi_getInfo(MEDIA_CUSTOM_CONTENT_INFO*) + setContentInfo(MEDIA_CUSTOM_SRC_TYPE_T, ci*). */
void _ZN13mediapipeline14CustomPipeline15loadSpi_getInfoEP25MEDIA_CUSTOM_CONTENT_INFO(void) {}
void _ZN13mediapipeline14CustomPipeline14setContentInfoE23MEDIA_CUSTOM_SRC_TYPE_TP25MEDIA_CUSTOM_CONTENT_INFO(void) {}
