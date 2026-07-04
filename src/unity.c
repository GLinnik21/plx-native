/* unity.c — single-TU build: the modules stay separate files on disk but compile
 * as ONE translation unit. This keeps the code organized while preserving the
 * monolith's thread-safe malloc (a multi-.o link on this webOS glibc target
 * fails to wire glibc's malloc-arena locks, corrupting the heap under threads). */
#include "gfx.c"
#include "text.c"
#include "system.c"
#include "stream.c"
#include "aq.c"
#include "mkv.c"
#include "img.c"
#include "pms.c"
#include "posters.c"
#include "playback.c"
#include "ui_home.c"
#include "main.c"
