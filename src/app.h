#ifndef PLEXPOC_APP_H
#define PLEXPOC_APP_H
#include <stdio.h>                 /* FILE */

/* (1) config.local.h (gitignored real secrets) overrides these placeholders.
 *     Resolves relative to app.h in src/, so src/config.local.h is found. */
#if defined(__has_include)
#  if __has_include("config.local.h")
#    include "config.local.h"
#  endif
#endif
#ifndef PMS_HOST
#  define PMS_HOST  "YOUR_PMS_HOST"
#endif
#ifndef PMS_PORT
#  define PMS_PORT  32400
#endif
#ifndef PMS_TOKEN
#  define PMS_TOKEN "YOUR_PLEX_TOKEN"
#endif
#ifndef DEMO_STREAM_URL
#  define DEMO_STREAM_URL "http://YOUR_PMS_HOST:32400/library/parts/0/0/file.mkv?X-Plex-Token=YOUR_PLEX_TOKEN"
#endif
#define RESUME_REWIND_NS (5LL * 1000000000LL)

/* (2) fixed panel geometry — gfx uniform, text, HUD, gallery all author at 1080p */
#define SCR_W 1920
#define SCR_H 1080

/* (3) the one process-wide global: the event/diagnostic log, DEFINED once as
 *     FILE *elogf = NULL; in main.c and read by nearly every UI/glue module. */
extern FILE *elogf;

#endif /* PLEXPOC_APP_H */
