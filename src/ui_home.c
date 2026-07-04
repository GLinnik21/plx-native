/* ui_home.c — gallery/home model + view + navigation (see ui_home.h). */
#define SDL_MAIN_HANDLED
#include <SDL2/SDL.h>      /* SDLK_* keysyms for home_move_focus */
#include "app.h"
#include "gfx.h"
#include "text.h"
#include "pms.h"
#include "posters.h"
#include "ui_home.h"
#include <string.h>
#include <math.h>
#include <stdio.h>

/* ---- home animation state (shared: fr/fc/snapTarget; rest private) ---- */
int   fr = 0, fc = 0;             /* focused row/col */
float snapTarget = 0;             /* 0 = big-picture hero, 1 = grid */
static float scale[ROWS][COLS], scaleV[ROWS][COLS];
static float scrollX[ROWS], scrollXV[ROWS];
static float scrollY, scrollYV;
static float snapPos, snapVel;
static float colTop[ROWS][COLS][4], colBot[ROWS][COLS][4];
static float bgPhase;

/* map a grid cell to a catalog movie (MVP: flat all-movies grid, row-major) */
pms_movie *movie_at(int r, int c) {
    int idx = r * COLS + c;
    return (idx >= 0 && idx < pms_nmovies) ? &pms_movies[idx] : NULL;
}

/* draw a movie's poster (thumb) in a card rect; dark skeleton until it loads */
static void draw_poster(pms_movie *m, float cx, float cy, float w, float h, float rad) {
    static const float tint[4] = {1.0f, 1.0f, 1.0f, 1.0f};
    static const float skT[4]  = {0.13f, 0.14f, 0.17f, 1.0f}, skB[4] = {0.08f, 0.09f, 0.11f, 1.0f};
    if (m && m->thumb[0]) {
        char key[352]; poster_key(key, sizeof key, m->thumb, 250, 375, 0);
        GLuint t = poster_get(key);
        if (t) { draw_tex(t, cx, cy, w, h, rad, tint); return; }
    }
    draw_rect(cx, cy, w, h, 0, rad, skT, skB, 0);
}

void home_init(void) {
    for (int r = 0; r < ROWS; r++)
        for (int c = 0; c < COLS; c++) scale[r][c] = 1.0f;
    /* card colors */
    for (int r = 0; r < ROWS; r++)
        for (int c = 0; c < COLS; c++) {
            float h = (float)((r * 67 + c * 31) % 360);
            hsv(h, 0.55f, 0.50f, colTop[r][c]);
            hsv(h + 18.0f, 0.65f, 0.28f, colBot[r][c]);
        }
}

/* vertical move keeps VISUAL alignment: pick the card under the focused
 * one given both rows' scroll offsets (Apple TV behavior) */
static void vert_move(int dir) {
    int nr = fr + (dir);
    float cx = MARGIN_X + fc * (CARD_W + GAP) - scrollX[fr] + CARD_W * 0.5f;
    int nc = (int)((cx - MARGIN_X - CARD_W * 0.5f + scrollX[nr])
                   / (CARD_W + GAP) + 0.5f);
    if (nc < 0) nc = 0;
    if (nc > COLS - 1) nc = COLS - 1;
    fr = nr; fc = nc;
}

void home_move_focus(unsigned s) {
    if ((s) == (unsigned)SDLK_LEFT && fc > 0) fc--;
    else if ((s) == (unsigned)SDLK_RIGHT && fc < COLS - 1) fc++;
    else if ((s) == (unsigned)SDLK_UP && fr > 0) vert_move(-1);
    else if ((s) == (unsigned)SDLK_DOWN && fr < ROWS - 1) vert_move(1);
}

void home_pointer_focus(float mx, float my) {
    for (int r = 0; r < ROWS; r++) {
        float rowY = CONTENT_Y + r * ROW_PITCH - scrollY +
                     ROW_TITLE_H + 18;
        if (my < rowY || my > rowY + CARD_H) continue;
        for (int c = 0; c < COLS; c++) {
            float x = MARGIN_X + c * (CARD_W + GAP) - scrollX[r];
            if (mx >= x && mx <= x + CARD_W) { fr = r; fc = c; }
        }
    }
}

void home_wheel(int dy) {
    if (dy < 0 && fr < ROWS - 1) fr++;
    else if (dy > 0 && fr > 0) fr--;
}

void home_update(float dt) {
    bgPhase += dt * 0.15f;

    /* springs */
    for (int r = 0; r < ROWS; r++)
        for (int c = 0; c < COLS; c++)
            spring(&scale[r][c], &scaleV[r][c],
                   (r == fr && c == fc) ? 1.055f : 1.0f, 320.0f, dt);
    float targetSX = fc * (CARD_W + GAP) - 0.0f;
    if (targetSX < 0) targetSX = 0;
    float maxSX = COLS * (CARD_W + GAP) - GAP - (SCR_W - 2 * MARGIN_X);
    if (targetSX > maxSX) targetSX = maxSX;
    /* keep focused card near left third */
    float want = fc * (CARD_W + GAP) - (CARD_W + GAP);
    if (want < 0) want = 0;
    if (want > maxSX) want = maxSX;
    spring(&scrollX[fr], &scrollXV[fr], want, 170.0f, dt);
    float wantY = fr * ROW_PITCH - ROW_PITCH * 0.6f;
    if (wantY < 0) wantY = 0;
    float maxY = ROWS * ROW_PITCH - (SCR_H - CONTENT_Y) + 60.0f;
    if (maxY < 0) maxY = 0;
    if (wantY > maxY) wantY = maxY;
    spring(&scrollY, &scrollYV, wantY, 170.0f, dt);

    spring(&snapPos, &snapVel, snapTarget, 200.0f, dt);   /* hero <-> grid snap */
}

void home_draw(void) {
    /* dark base — the hero backdrop covers it once the art texture loads */
    glClearColor(0.03f, 0.03f, 0.045f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);

    /* the home screen is one continuum driven by snapPos (0 = hero, 1 = grid) */
    float sp = snapPos;
    float heroA = 1.0f - sp / 0.55f;  if (heroA < 0) heroA = 0;  if (heroA > 1) heroA = 1;
    float shelfTopY = 828.0f + (150.0f - 828.0f) * sp;   /* PEEK_Y -> GRID_TOP_Y */
    pms_movie *hero = movie_at(0, 0);

    /* --- ambient blur-color wash (Plex UltraBlurColors): the soft background the
     * artwork melts into; it IS the grid background once the art fades away.
     * Only drawn where it shows (grid/transition, or while the backdrop loads) —
     * in the hero view the opaque backdrop covers it, so skip that fill-rate. --- */
    GLuint bt = 0; char bk[352];
    if (hero && hero->art[0]) { poster_key(bk, sizeof bk, hero->art, 1280, 720, 0); bt = poster_get(bk); }
    if (hero && hero->has_blur && (sp > 0.004f || !bt))
        draw_ambient(0, 0, SCR_W, SCR_H, 0.55f,
                     hero->blur[0], hero->blur[1], hero->blur[2], hero->blur[3]);
    /* --- hero backdrop (art) over the wash: full in hero, fades + parallaxes away
     * as the grid rises so the smooth gradient shows through. --- */
    if (bt && sp < 0.996f) {                             /* skip once fully faded */
        float ba = 1.0f - sp;
        float bdTint[4] = {1.0f, 1.0f, 1.0f, ba};
        draw_tex(bt, 0, -sp * (SCR_H - 120.0f), SCR_W, SCR_H, 0, bdTint);
    }
    /* bottom scrim for hero-text legibility; only in the hero view */
    if (heroA > 0.01f) {
        float sa = 0.30f + 0.64f * heroA;
        float scrimT[4] = {0.02f, 0.02f, 0.03f, 0.0f}, scrimB[4] = {0.02f, 0.02f, 0.03f, sa};
        draw_rect(0, SCR_H * 0.46f, SCR_W, SCR_H * 0.54f, 0, 0, scrimT, scrimB, 0);
    }

    /* --- hero content (low-left), fades out as the grid rises --- */
    if (hero && heroA > 0.01f) {
        float tx = MARGIN_X, titleY = 510.0f;
        float wA[4] = {0.97f, 0.98f, 0.99f, heroA};
        float dA[4] = {0.70f, 0.73f, 0.78f, heroA};
        /* title: the movie's clearLogo (transparent PNG) if loaded, else bold text */
        GLuint lt = 0; int lw = 0, lh = 0;
        if (hero->rk[0]) {
            char lpath[72], lk[352];
            snprintf(lpath, sizeof lpath, "/library/metadata/%s/clearLogo", hero->rk);
            poster_key(lk, sizeof lk, lpath, 600, 240, 1);   /* png=1 (transparent) */
            lt = poster_get(lk); poster_wh(lk, &lw, &lh);
        }
        if (lt && lh > 0) {
            float H = 96.0f, W = H * (float)lw / (float)lh;
            if (W > 660.0f) { W = 660.0f; H = W * (float)lh / (float)lw; }
            draw_tex(lt, tx, titleY + 80.0f - H, W, H, 0, wA);   /* bottom-anchored */
        } else {
            draw_text(hero->title, tx, titleY, 66, wA, 0, 1);
        }
        char meta[96];
        snprintf(meta, sizeof meta, "Movie \xc2\xb7 %d \xc2\xb7 %s",
                 hero->year, hero->rating[0] ? hero->rating : "NR");
        draw_text(meta, tx, titleY + 92, 26, dA, 0, 0);
        /* synopsis wrapped to two lines on a word boundary */
        if (hero->summary[0]) {
            const char *s = hero->summary; int n = (int)strlen(s);
            char l1[88] = {0}, l2[96] = {0}; int brk = n;
            if (n > 62) { brk = 62; while (brk > 24 && s[brk] != ' ') brk--; }
            int c1 = brk < (int)sizeof l1 - 1 ? brk : (int)sizeof l1 - 1;
            memcpy(l1, s, c1); l1[c1] = 0;
            draw_text(l1, tx, titleY + 128, 24, dA, 0, 0);
            if (brk < n) {
                const char *s2 = s + brk + 1; int m = (int)strlen(s2);
                int c2 = m; if (m > 66) { c2 = 66; while (c2 > 24 && s2[c2] != ' ') c2--; }
                if (c2 > (int)sizeof l2 - 4) c2 = (int)sizeof l2 - 4;
                memcpy(l2, s2, c2); l2[c2] = 0;
                if (c2 < m) strcat(l2, "\xe2\x80\xa6");   /* … */
                draw_text(l2, tx, titleY + 158, 24, dA, 0, 0);
            }
        }
        /* Play pill (primary) — triangle + label centered as a group */
        float pillH = 60, pillW = 168, pillY = titleY + 200;
        float pillC[4] = {0.97f, 0.98f, 0.99f, heroA}, ink[4] = {0.05f, 0.06f, 0.08f, heroA};
        draw_rrect(tx, pillY, pillW, pillH, pillH * 0.5f, pillH * 0.5f, pillC);
        float triH = pillH * 0.40f;
        draw_ptri(tx + 40, pillY + (pillH - triH) * 0.5f, triH, triH, ink);
        draw_text("Play", tx + 76, pillY + (pillH - 30) * 0.5f - 1, 30, ink, 0, 1);
        /* circular secondary buttons: add / info / next (glyphs centered) */
        float cD = 60, cGap = 20;
        float circ[4] = {0.42f, 0.44f, 0.50f, 0.5f * heroA}, gly[4] = {0.92f, 0.94f, 0.97f, heroA};
        const char *ic[3] = {"+", "i", ">"};
        for (int b = 0; b < 3; b++) {
            float bx = tx + pillW + cGap + b * (cD + cGap);
            draw_rect(bx, pillY, cD, cD, 0, cD * 0.5f, circ, circ, 0);
            draw_text(ic[b], bx + cD * 0.5f, pillY + (cD - 32) * 0.5f - 2, 32, gly, 1, 1);
        }
        /* page dots */
        float dotY = pillY + pillH + 24;
        for (int d = 0; d < 8; d++) {
            float dw = (d == 0) ? 26.0f : 11.0f;
            float dc[4] = {0.85f, 0.87f, 0.9f, (d == 0 ? 0.95f : 0.35f) * heroA};
            draw_rect(tx + d * 20.0f, dotY, dw, 11, 0, 5.5f, dc, dc, 0);
        }
    }

    /* --- shelves: peek at the bottom in hero mode, full grid when snapped --- */
    for (int r = 0; r < ROWS; r++) {
        float rowY = shelfTopY + r * ROW_PITCH - scrollY * sp;
        if (rowY > SCR_H || rowY + CARD_H < 0) continue;
        if (!movie_at(r, 0)) continue;
        for (int c = 0; c < COLS; c++) {
            if (r == fr && c == fc && sp > 0.5f) continue;   /* focused drawn last (grid) */
            pms_movie *m = movie_at(r, c);
            if (!m) continue;
            float x = MARGIN_X + c * (CARD_W + GAP) - scrollX[r] * sp;
            if (x > SCR_W || x + CARD_W < -GLOW_PAD) continue;
            float s = scale[r][c];
            float w = CARD_W * s, h = CARD_H * s;
            float cx = x - (w - CARD_W) / 2, cy = (rowY + 12) - (h - CARD_H) / 2;
            draw_poster(m, cx, cy, w, h, 14.0f * s);
        }
    }
    /* focused card ring + label — only in grid mode */
    if (sp > 0.5f) {
        pms_movie *m = movie_at(fr, fc);
        float rowY = shelfTopY + fr * ROW_PITCH - scrollY * sp;
        float x = MARGIN_X + fc * (CARD_W + GAP) - scrollX[fr] * sp;
        float s = scale[fr][fc];
        float w = CARD_W * s, h = CARD_H * s;
        float cx = x - (w - CARD_W) / 2, cy = (rowY + 12) - (h - CARD_H) / 2;
        draw_poster(m, cx, cy, w, h, 14.0f * s);
        float clear0[4] = {0, 0, 0, 0};
        draw_rect(cx - GLOW_PAD, cy - GLOW_PAD, w + 2 * GLOW_PAD, h + 2 * GLOW_PAD,
                  GLOW_PAD, 14.0f * s, clear0, clear0, (s - 1.0f) / 0.055f);
        if (m) {
            float lc[4] = {0.96f, 0.97f, 0.98f, 1.0f};
            draw_text(m->title, cx + w * 0.5f, cy + h + 12, 26, lc, 1, 1);
        }
    }
}
