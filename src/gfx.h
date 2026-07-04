#ifndef PLEXPOC_GFX_H
#define PLEXPOC_GFX_H
/* GLES2 rendering foundation: shader compile, the SDF rounded-rect/triangle
 * program (the resting 'prog' left bound at rest), the 4-corner ambient-gradient
 * program, the textured-quad/image program, the shared unit-quad VBO bound once
 * in init_gl, the low-level draw primitives, seven-segment FPS digits, and
 * hsv()/spring() math. gfx_compile()/gfx_use_base() let text.c compile its own
 * program and restore the base program without touching gfx's private handles. */
#include <GLES2/gl2.h>

/* Compile a shader (fatal exit on error). Shared with text.c. */
GLuint gfx_compile(GLenum type, const char *src);
/* init_gl: create+bind the resting program, the shared unit-quad VBO (attrib 0),
 * and the ambient program. MUST run first, before init_text/init_image/any draw. */
void init_gl(void);
/* iprog: the textured poster/logo/backdrop program. */
void init_image(void);
/* Re-bind the resting SDF program (call after entering another program). */
void gfx_use_base(void);

void draw_rect(float x, float y, float w, float h, float pad,
               float radius, const float top[4], const float bot[4], float focus);
void draw_rrect(float x, float y, float w, float h, float radL,
                float radR, const float col[4]);
void draw_ptri(float x, float y, float w, float h, const float col[4]);
void draw_ambient(float x, float y, float w, float h, float dim,
                  const float tl[3], const float tr[3],
                  const float br[3], const float bl[3]);
void draw_tex(GLuint tex, float x, float y, float w, float h,
              float radius, const float tint[4]);
void draw_number(int n, float right_x, float y, float s, const float col[4]);
void hsv(float h, float s, float v, float out[4]);
void spring(float *pos, float *vel, float target, float k, float dt);

#endif /* PLEXPOC_GFX_H */
