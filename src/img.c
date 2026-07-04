/* img.c — stb_image decode + GL texture upload (see img.h).
 * The ~8k-line stb_image implementation compiles in exactly this ONE TU. */
#define STB_IMAGE_IMPLEMENTATION
#define STBI_ONLY_JPEG
#define STBI_ONLY_PNG
#define STBI_NO_STDIO           /* we only ever decode from memory */
#define STBI_NO_FAILURE_STRINGS /* smaller; failure is just NULL */
#include "stb_image.h"
#include "img.h"

/* Decode JPEG/PNG bytes to tightly-packed RGBA8 (comp forced to 4).
 * Thread-safe (no shared state). Returns NULL on failure; free with img_free(). */
unsigned char *img_decode_rgba(const unsigned char *buf, int len, int *w, int *h) {
    int comp = 0;
    if (!buf || len <= 0) return NULL;
    return stbi_load_from_memory(buf, len, w, h, &comp, 4);
}

void img_free(unsigned char *px) { if (px) stbi_image_free(px); }

/* Upload RGBA8 pixels as a 2D texture. MUST run on the GL (main) thread.
 * Mirrors the app's existing glyph-texture params (LINEAR, CLAMP_TO_EDGE,
 * UNPACK_ALIGNMENT 4). Returns the texture id, or 0 on failure. */
GLuint img_upload_rgba(const unsigned char *px, int w, int h) {
    if (!px || w <= 0 || h <= 0) return 0;
    GLuint t = 0;
    glGenTextures(1, &t);
    if (!t) return 0;
    glBindTexture(GL_TEXTURE_2D, t);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, px);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    return t;
}

/* Decode + upload in one call (main/GL thread only). Convenience for the
 * synchronous path; the async pipeline calls decode and upload separately. */
GLuint img_tex_from_memory(const unsigned char *buf, int len, int *out_w, int *out_h) {
    int w = 0, h = 0;
    unsigned char *px = img_decode_rgba(buf, len, &w, &h);
    if (!px) return 0;
    GLuint t = img_upload_rgba(px, w, h);
    img_free(px);
    if (out_w) *out_w = w;
    if (out_h) *out_h = h;
    return t;
}
