/* text.c — SDL2_ttf text rendering: font cache + glyph-texture LRU + draw_text. */
#define SDL_MAIN_HANDLED
#include <SDL2/SDL.h>       /* SDL_Color / SDL_Surface */
#include "app.h"
#include "gfx.h"            /* gfx_compile, gfx_use_base */
#include "text.h"
#include <GLES2/gl2.h>
#include <string.h>

/* SDL2_ttf (real impl on the TV); SDL_Color/SDL_Surface come from SDL.h */
typedef struct _TTF_Font TTF_Font;
extern int  TTF_Init(void);
extern TTF_Font *TTF_OpenFont(const char *file, int ptsize);
extern SDL_Surface *TTF_RenderUTF8_Blended(TTF_Font *font, const char *text, SDL_Color fg);
extern int  TTF_SizeUTF8(TTF_Font *font, const char *text, int *w, int *h);
extern void TTF_SetFontStyle(TTF_Font *font, int style);   /* 0x01 = BOLD */

/* textured vertex shader (private copy; gfx.c keeps its own copy for iprog). */
static const char *VS_TEXT =
    "attribute vec2 a_pos;\n"
    "uniform vec4 u_trect;\n"
    "uniform vec2 u_tscreen;\n"
    "varying vec2 v_tuv;\n"
    "void main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;\n"
    "  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }\n";
static const char *FS_TEXT =
    "precision mediump float;\n"
    "varying vec2 v_tuv;\n"
    "uniform sampler2D u_tex;\n"
    "uniform vec4 u_tcol;\n"       /* text color; texture alpha = glyph coverage */
    "void main(){ float a=texture2D(u_tex,v_tuv).a; gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }\n";

static GLuint tprog;
static GLint  tl_rect, tl_screen, tl_col, tl_tex;
static TTF_Font *g_fonts[80], *g_fonts_b[80];   /* regular / synthesized-bold per ptsize */
static int g_text_ok = 0;

#define APPDIR_PATH "/media/developer/apps/usr/palm/applications/com.glin.plexpoc/"
#define APP_FONT      APPDIR_PATH "appfont.ttf"        /* Arial Regular */
#define APP_FONT_BOLD APPDIR_PATH "appfont-bold.ttf"   /* Arial Bold (real face) */
static TTF_Font *font_at(int sz, int bold) {
    if (sz < 8) sz = 8; if (sz > 79) sz = 79;
    TTF_Font **arr = bold ? g_fonts_b : g_fonts;
    if (!arr[sz]) {
        arr[sz] = TTF_OpenFont(bold ? APP_FONT_BOLD : APP_FONT, sz);
        if (!arr[sz]) {   /* fallbacks: regular app font, then DroidSans */
            arr[sz] = TTF_OpenFont(APP_FONT, sz);
            if (!arr[sz]) arr[sz] = TTF_OpenFont("/usr/share/fonts/DroidSans.ttf", sz);
            if (arr[sz] && bold) TTF_SetFontStyle(arr[sz], 0x01);
        }
    }
    return arr[sz];
}
void init_text(void) {
    if (TTF_Init() != 0) { if (elogf){fprintf(elogf,"TTF_Init failed\n");fflush(elogf);} return; }
    tprog = glCreateProgram();
    glAttachShader(tprog, gfx_compile(GL_VERTEX_SHADER, VS_TEXT));
    glAttachShader(tprog, gfx_compile(GL_FRAGMENT_SHADER, FS_TEXT));
    glBindAttribLocation(tprog, 0, "a_pos");
    glLinkProgram(tprog);
    GLint ok = 0; glGetProgramiv(tprog, GL_LINK_STATUS, &ok);
    if (!ok) { if (elogf){fprintf(elogf,"text prog link failed\n");fflush(elogf);} return; }
    tl_rect   = glGetUniformLocation(tprog, "u_trect");
    tl_screen = glGetUniformLocation(tprog, "u_tscreen");
    tl_col    = glGetUniformLocation(tprog, "u_tcol");
    tl_tex    = glGetUniformLocation(tprog, "u_tex");
    if (font_at(28, 0)) g_text_ok = 1;
    gfx_use_base();
    if (elogf) { fprintf(elogf, "init_text ok=%d\n", g_text_ok); fflush(elogf); }
}

#define TCACHE 48
static struct { char s[96]; int sz; int bold; GLuint tex; int w, h; unsigned use; } tcache[TCACHE];
static unsigned tclock = 0;
/* returns GL texture id (0 on failure) and sets w,h out-params */
static GLuint text_tex(const char *s, int sz, int bold, int *w, int *h) {
    for (int i = 0; i < TCACHE; i++)
        if (tcache[i].tex && tcache[i].sz == sz && tcache[i].bold == bold &&
            strcmp(tcache[i].s, s) == 0) {
            tcache[i].use = ++tclock; *w = tcache[i].w; *h = tcache[i].h; return tcache[i].tex; }
    TTF_Font *f = font_at(sz, bold); if (!f) return 0;
    SDL_Color white = {255, 255, 255, 255};
    SDL_Surface *surf = TTF_RenderUTF8_Blended(f, s, white);
    if (!surf) return 0;
    GLuint tex; glGenTextures(1, &tex); glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, surf->w, surf->h, 0,
                 GL_RGBA, GL_UNSIGNED_BYTE, surf->pixels);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    int sw = surf->w, sh = surf->h;
    SDL_FreeSurface(surf);
    int slot = 0; unsigned oldest = ~0u;
    for (int i = 0; i < TCACHE; i++) {
        if (!tcache[i].tex) { slot = i; break; }
        if (tcache[i].use < oldest) { oldest = tcache[i].use; slot = i; }
    }
    if (tcache[slot].tex) glDeleteTextures(1, &tcache[slot].tex);
    strncpy(tcache[slot].s, s, sizeof tcache[slot].s - 1);
    tcache[slot].s[sizeof tcache[slot].s - 1] = 0;
    tcache[slot].sz = sz; tcache[slot].bold = bold;
    tcache[slot].tex = tex; tcache[slot].w = sw; tcache[slot].h = sh;
    tcache[slot].use = ++tclock;
    *w = sw; *h = sh; return tex;
}
/* align: 0 left, 1 center, 2 right (x is the anchor edge). returns text width. */
float draw_text(const char *s, float x, float y, int sz,
                const float col[4], int align, int bold) {
    if (!g_text_ok || !s || !s[0]) return 0;
    int w = 0, h = 0; GLuint tex = text_tex(s, sz, bold, &w, &h);
    if (!tex) return 0;
    float dx = align == 1 ? x - w * 0.5f : align == 2 ? x - w : x;
    glUseProgram(tprog);
    glUniform2f(tl_screen, (float)SCR_W, (float)SCR_H);
    glUniform4fv(tl_col, 1, col);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, tex);
    glUniform1i(tl_tex, 0);
    glUniform4f(tl_rect, dx, y, (float)w, (float)h);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    gfx_use_base();   /* restore rect program for subsequent draw_rect */
    return (float)w;
}
