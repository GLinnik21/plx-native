#ifndef PLEXPOC_UI_HOME_H
#define PLEXPOC_UI_HOME_H
/* Gallery/home model + view + navigation: hero + peeking-shelf continuum,
 * scroll-snap, spring-animated focus scale + per-row scroll, D-pad/pointer/wheel
 * navigation. Owns the home animation state as file-scope globals in ui_home.c;
 * main's controller loop shares fr/fc/snapTarget via the externs below. */
#include "pms.h"      /* pms_movie (movie_at return type) */

/* ---- gallery layout (authored at 1080p) ---- */
#define ROWS 5
#define COLS 10
#define CARD_W 250.0f    /* portrait 2:3 poster card */
#define CARD_H 375.0f
#define GAP 30.0f
#define MARGIN_X 90.0f
#define ROW_TITLE_H 30.0f
#define ROW_PITCH (CARD_H + ROW_TITLE_H + 54.0f)
#define CONTENT_Y 200.0f
#define GLOW_PAD 48.0f /* extra quad space around card for glow/shadow */

/* shared with main's controller loop */
extern int   fr, fc;        /* focused row/col */
extern float snapTarget;    /* 0 = big-picture hero, 1 = grid */

/* map a grid cell to a catalog movie (MVP: flat all-movies grid, row-major) */
pms_movie *movie_at(int r, int c);
void home_init(void);                        /* card colors + focus scale reset */
void home_update(float dt);                  /* springs + bg phase */
void home_draw(void);                         /* hero + shelves + focus ring */
void home_move_focus(unsigned sym);           /* D-pad grid navigation */
void home_pointer_focus(float mx, float my);  /* pointer hover hit-test */
void home_wheel(int dy);                      /* wheel row up/down */

#endif /* PLEXPOC_UI_HOME_H */
