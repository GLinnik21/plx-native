/* gfx.c — GLES2 rendering foundation (see gfx.h). */
#include "app.h"
#include "gfx.h"
#include <GLES2/gl2.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static const char *VS_SRC =
    "attribute vec2 a_pos;\n"
    "uniform vec4 u_rect;\n"   /* x,y,w,h in screen px */
    "uniform vec2 u_screen;\n"
    "varying vec2 v_uv;\n"
    "void main(){\n"
    "  v_uv = a_pos;\n"
    "  vec2 px = u_rect.xy + a_pos * u_rect.zw;\n"
    "  vec2 ndc = px / u_screen * 2.0 - 1.0;\n"
    "  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);\n"
    "}\n";

static const char *FS_SRC =
    "precision mediump float;\n"
    "varying vec2 v_uv;\n"
    "uniform vec2 u_size;\n"    /* quad size px */
    "uniform float u_pad;\n"    /* inset from quad edge to card edge */
    "uniform float u_radius;\n"
    "uniform vec4 u_colTop;\n"
    "uniform vec4 u_colBot;\n"
    "uniform float u_focus;\n"  /* 0..1 focus ring+glow */
    "uniform float u_shape;\n"  /* 0 rounded rect, 1 right-pointing triangle */
    "uniform float u_radR;\n"   /* right-corner radius (u_radius = left) */
    "float sdBox(vec2 p, vec2 b, float r){\n"
    "  vec2 q = abs(p) - b + vec2(r);\n"
    "  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;\n"
    "}\n"
    "void main(){\n"
    "  if (u_shape > 0.5) {\n"                       /* play triangle in the quad */
    "    float tri = step(0.5*v_uv.x, v_uv.y) * step(v_uv.y, 1.0 - 0.5*v_uv.x);\n"
    "    gl_FragColor = vec4(u_colTop.rgb * tri, tri * u_colTop.a);\n"
    "    return;\n"
    "  }\n"
    "  vec2 p = (v_uv - 0.5) * u_size;\n"
    "  vec2 hsz = u_size * 0.5 - vec2(u_pad);\n"
    "  float rad = (p.x > 0.0) ? u_radR : u_radius;\n"
    "  float d = sdBox(p, hsz, rad);\n"
    "  vec4 fill = mix(u_colTop, u_colBot, v_uv.y);\n"
    "  float aFill = 1.0 - smoothstep(-1.0, 1.0, d);\n"
    "  vec3 rgb = fill.rgb * aFill;\n"
    "  float a = aFill * fill.a;\n"
    "  if (u_focus > 0.001) {\n"
    "    float ring = (1.0 - smoothstep(1.5, 4.0, abs(d - 5.0))) * u_focus;\n"
    "    float glow = exp(-max(d, 0.0) / 14.0) * 0.40 * u_focus * step(0.0, d);\n"
    "    rgb += vec3(1.0) * ring + vec3(0.85, 0.9, 1.0) * glow;\n"
    "    a = max(a, max(ring, glow));\n"
    "  }\n"
    "  gl_FragColor = vec4(rgb, a);\n"
    "}\n";

static GLuint prog;
static GLint loc_rect, loc_screen, loc_size, loc_pad, loc_radius,
             loc_colTop, loc_colBot, loc_focus, loc_shape, loc_radR;

/* ambient program: a soft bilinear gradient between 4 corner colors (Plex
 * UltraBlurColors) — the smooth wash the artwork melts into. Reuses VS_SRC. */
static const char *FS_AMBIENT =
    "precision mediump float;\n"
    "varying vec2 v_uv;\n"
    "uniform vec4 u_atl, u_atr, u_abr, u_abl;\n"
    "void main(){\n"
    "  vec3 top = mix(u_atl.rgb, u_atr.rgb, v_uv.x);\n"
    "  vec3 bot = mix(u_abl.rgb, u_abr.rgb, v_uv.x);\n"
    "  gl_FragColor = vec4(mix(top, bot, v_uv.y), 1.0);\n"
    "}\n";
static GLuint aprog;
static GLint al_rect, al_screen, al_tl, al_tr, al_br, al_bl;

/* textured vertex shader (private copy for iprog; text.c keeps its own copy). */
static const char *VS_IMG =
    "attribute vec2 a_pos;\n"
    "uniform vec4 u_trect;\n"
    "uniform vec2 u_tscreen;\n"
    "varying vec2 v_tuv;\n"
    "void main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;\n"
    "  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }\n";

GLuint gfx_compile(GLenum type, const char *src) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, NULL);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[1024];
        glGetShaderInfoLog(s, sizeof log, NULL, log);
        fprintf(stderr, "shader error: %s\n", log);
        exit(1);
    }
    return s;
}

void gfx_use_base(void) { glUseProgram(prog); }

void init_gl(void) {
    prog = glCreateProgram();
    glAttachShader(prog, gfx_compile(GL_VERTEX_SHADER, VS_SRC));
    glAttachShader(prog, gfx_compile(GL_FRAGMENT_SHADER, FS_SRC));
    glBindAttribLocation(prog, 0, "a_pos");
    glLinkProgram(prog);
    GLint ok = 0;
    glGetProgramiv(prog, GL_LINK_STATUS, &ok);
    if (!ok) { fprintf(stderr, "link failed\n"); exit(1); }
    glUseProgram(prog);
    loc_rect   = glGetUniformLocation(prog, "u_rect");
    loc_screen = glGetUniformLocation(prog, "u_screen");
    loc_size   = glGetUniformLocation(prog, "u_size");
    loc_pad    = glGetUniformLocation(prog, "u_pad");
    loc_radius = glGetUniformLocation(prog, "u_radius");
    loc_colTop = glGetUniformLocation(prog, "u_colTop");
    loc_colBot = glGetUniformLocation(prog, "u_colBot");
    loc_focus  = glGetUniformLocation(prog, "u_focus");
    loc_shape  = glGetUniformLocation(prog, "u_shape");
    loc_radR   = glGetUniformLocation(prog, "u_radR");
    glUniform2f(loc_screen, (float)SCR_W, (float)SCR_H);

    static const GLfloat quad[8] = {0,0, 1,0, 0,1, 1,1};
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof quad, quad, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);

    aprog = glCreateProgram();
    glAttachShader(aprog, gfx_compile(GL_VERTEX_SHADER, VS_SRC));
    glAttachShader(aprog, gfx_compile(GL_FRAGMENT_SHADER, FS_AMBIENT));
    glBindAttribLocation(aprog, 0, "a_pos");
    glLinkProgram(aprog);
    al_rect   = glGetUniformLocation(aprog, "u_rect");
    al_screen = glGetUniformLocation(aprog, "u_screen");
    al_tl = glGetUniformLocation(aprog, "u_atl"); al_tr = glGetUniformLocation(aprog, "u_atr");
    al_br = glGetUniformLocation(aprog, "u_abr"); al_bl = glGetUniformLocation(aprog, "u_abl");
    glUseProgram(prog);

    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
}

void draw_rect(float x, float y, float w, float h, float pad,
               float radius, const float top[4], const float bot[4],
               float focus) {
    glUniform4f(loc_rect, x, y, w, h);
    glUniform2f(loc_size, w, h);
    glUniform1f(loc_pad, pad);
    glUniform1f(loc_radius, radius);
    glUniform1f(loc_radR, radius);
    glUniform4fv(loc_colTop, 1, top);
    glUniform4fv(loc_colBot, 1, bot);
    glUniform1f(loc_focus, focus);
    glUniform1f(loc_shape, 0.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

/* full-rect bilinear gradient from 4 corner colors (UltraBlurColors ambient).
 * `dim` scales brightness so it reads as a soft dark wash behind the content. */
void draw_ambient(float x, float y, float w, float h, float dim,
                  const float tl[3], const float tr[3],
                  const float br[3], const float bl[3]) {
    glUseProgram(aprog);
    glUniform2f(al_screen, (float)SCR_W, (float)SCR_H);
    glUniform4f(al_rect, x, y, w, h);
    glUniform4f(al_tl, tl[0]*dim, tl[1]*dim, tl[2]*dim, 1.0f);
    glUniform4f(al_tr, tr[0]*dim, tr[1]*dim, tr[2]*dim, 1.0f);
    glUniform4f(al_br, br[0]*dim, br[1]*dim, br[2]*dim, 1.0f);
    glUniform4f(al_bl, bl[0]*dim, bl[1]*dim, bl[2]*dim, 1.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glUseProgram(prog);
}

/* solid rect with independent left/right corner radii (radL, radR) */
void draw_rrect(float x, float y, float w, float h, float radL,
                float radR, const float col[4]) {
    glUniform4f(loc_rect, x, y, w, h);
    glUniform2f(loc_size, w, h);
    glUniform1f(loc_pad, 0.0f);
    glUniform1f(loc_radius, radL);
    glUniform1f(loc_radR, radR);
    glUniform4fv(loc_colTop, 1, col);
    glUniform4fv(loc_colBot, 1, col);
    glUniform1f(loc_focus, 0.0f);
    glUniform1f(loc_shape, 0.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

/* filled right-pointing play triangle inscribed in the given box */
void draw_ptri(float x, float y, float w, float h, const float col[4]) {
    glUniform4f(loc_rect, x, y, w, h);
    glUniform2f(loc_size, w, h);
    glUniform4fv(loc_colTop, 1, col);
    glUniform4fv(loc_colBot, 1, col);
    glUniform1f(loc_focus, 0.0f);
    glUniform1f(loc_shape, 1.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

void hsv(float h, float s, float v, float out[4]) {
    float c = v * s, hp = fmodf(h, 360.0f) / 60.0f;
    float x = c * (1.0f - fabsf(fmodf(hp, 2.0f) - 1.0f));
    float r = 0, g = 0, b = 0;
    if (hp < 1)      { r = c; g = x; }
    else if (hp < 2) { r = x; g = c; }
    else if (hp < 3) { g = c; b = x; }
    else if (hp < 4) { g = x; b = c; }
    else if (hp < 5) { r = x; b = c; }
    else             { r = c; b = x; }
    float m = v - c;
    out[0] = r + m; out[1] = g + m; out[2] = b + m; out[3] = 1.0f;
}

/* critically-damped spring step */
void spring(float *pos, float *vel, float target, float k, float dt) {
    /* critical-damping c = 2*sqrt(k); k is one of a couple constants, so memoize
     * instead of a sqrt per call (~52 spring updates/frame) */
    static float lastK = -1.0f, lastC = 0.0f;
    if (k != lastK) { lastK = k; lastC = 2.0f * sqrtf(k); }
    float c = lastC;
    float a = k * (target - *pos) - c * (*vel);
    *vel += a * dt;
    *pos += *vel * dt;
}

/* --- seven-segment FPS digits (quads) --- */
static const unsigned char SEG[10] = {
    0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F};

static void draw_digit(int d, float x, float y, float s, const float col[4]) {
    /* segments: 0 top,1 tr,2 br,3 bottom,4 bl,5 tl,6 mid */
    float w = 0.16f * s;
    struct { float x, y, w, h; } g[7] = {
        {0, 0, 1.0f, 0},       {1.0f, 0, 0, 0.5f}, {1.0f, 0.5f, 0, 0.5f},
        {0, 1.0f, 1.0f, 0},    {0, 0.5f, 0, 0.5f}, {0, 0, 0, 0.5f},
        {0, 0.5f, 1.0f, 0}};
    for (int i = 0; i < 7; i++) {
        if (!(SEG[d] >> i & 1)) continue;
        float sx = x + g[i].x * s - w / 2, sy = y + g[i].y * s - w / 2;
        float sw = g[i].w * s + w, sh = g[i].h * s + w;
        draw_rect(sx, sy, sw, sh, 2.0f, (w + 4) / 2 - 2, col, col, 0);
    }
}

void draw_number(int n, float right_x, float y, float s,
                 const float col[4]) {
    if (n < 0) n = 0;
    if (n > 999) n = 999;
    float adv = s + 0.55f * s;
    float x = right_x - adv;
    do {
        draw_digit(n % 10, x, y, s, col);
        n /= 10;
        x -= adv;
    } while (n > 0);
}

/* ---- image program: RGBA textures (posters/logos/backdrop) with rounded corners.
 * Reuses VS_IMG; FS_IMG samples full RGBA * tint and rounds via an SDF box, so one
 * shader serves opaque posters (a=1), transparent clearLogos, and the backdrop
 * (radius 0). Like draw_text it enters iprog and self-restores prog on exit. ---- */
static const char *FS_IMG =
    "precision mediump float;\n"
    "varying vec2 v_tuv;\n"
    "uniform sampler2D u_tex;\n"
    "uniform vec4 u_tint;\n"
    "uniform vec2 u_isize;\n"
    "uniform float u_iradius;\n"
    "float sdBox(vec2 p, vec2 b, float r){ vec2 q=abs(p)-b+vec2(r);\n"
    "  return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r; }\n"
    "void main(){\n"
    "  vec4 c = texture2D(u_tex, v_tuv);\n"
    "  vec2 p = (v_tuv-0.5)*u_isize;\n"
    "  float d = sdBox(p, u_isize*0.5, u_iradius);\n"
    "  float m = 1.0 - smoothstep(-1.0, 1.0, d);\n"
    "  gl_FragColor = vec4(c.rgb*u_tint.rgb, c.a*u_tint.a*m);\n"
    "}\n";
static GLuint iprog;
static GLint il_rect, il_screen, il_tint, il_size, il_radius, il_tex;
void init_image(void) {
    iprog = glCreateProgram();
    glAttachShader(iprog, gfx_compile(GL_VERTEX_SHADER, VS_IMG));   /* textured VS */
    glAttachShader(iprog, gfx_compile(GL_FRAGMENT_SHADER, FS_IMG));
    glBindAttribLocation(iprog, 0, "a_pos");
    glLinkProgram(iprog);
    GLint ok = 0; glGetProgramiv(iprog, GL_LINK_STATUS, &ok);
    if (!ok) { if (elogf){fprintf(elogf,"image prog link failed\n");fflush(elogf);} return; }
    il_rect   = glGetUniformLocation(iprog, "u_trect");
    il_screen = glGetUniformLocation(iprog, "u_tscreen");
    il_tint   = glGetUniformLocation(iprog, "u_tint");
    il_size   = glGetUniformLocation(iprog, "u_isize");
    il_radius = glGetUniformLocation(iprog, "u_iradius");
    il_tex    = glGetUniformLocation(iprog, "u_tex");
    glUseProgram(prog);
}
/* draw texture in px rect (x,y,w,h), rounded corners `radius`, multiplied by tint. */
void draw_tex(GLuint tex, float x, float y, float w, float h,
              float radius, const float tint[4]) {
    if (!tex) return;
    glUseProgram(iprog);
    glUniform2f(il_screen, (float)SCR_W, (float)SCR_H);
    glUniform4fv(il_tint, 1, tint);
    glUniform2f(il_size, w, h);
    glUniform1f(il_radius, radius);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, tex);
    glUniform1i(il_tex, 0);
    glUniform4f(il_rect, x, y, w, h);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glUseProgram(prog);   /* restore */
}
