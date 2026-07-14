#ifndef PLXNATIVE_APP_H
#define PLXNATIVE_APP_H
#include <stdio.h>                 /* FILE */

/* (1) config.local.h (gitignored, dev-only) overrides the host placeholder. NO token macro:
 *     the binary carries no credentials — PMS access comes from the signed-in session, or from
 *     the /tmp/plxnative-token dev trigger for automated runs (the token in config.local.h is read by
 *     tests/run.py on the HOST, never compiled in). */
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
#define RESUME_REWIND_NS (5LL * 1000000000LL)

/* (2) fixed panel geometry — gfx uniform, text, HUD, gallery all author at 1080p */
#define SCR_W 1920
#define SCR_H 1080

/* (3) the one process-wide global: the event/diagnostic log, DEFINED once as
 *     FILE *elogf = NULL; in main.c and read by nearly every UI/glue module. */
extern FILE *elogf;

#endif /* PLXNATIVE_APP_H */
