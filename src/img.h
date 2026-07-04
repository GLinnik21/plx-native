#ifndef PLEXPOC_IMG_H
#define PLEXPOC_IMG_H
/* Image decode (Plex JPEG posters / PNG clearLogos) -> RGBA8 -> GL texture.
 *
 * Vendors stb_image (implementation compiled ONLY in img.c). Split into decode vs
 * upload on purpose: posters are fetched over HTTP and DECODED on a background
 * thread, but the GL UPLOAD (img_upload_rgba) MUST run on the main/GL thread — the
 * GLES context is single-threaded. See the poster pipeline in posters.c.
 *
 * Only JPEG (posters) + PNG (transparent clearLogo title art) are compiled in. */
#include <GLES2/gl2.h>

/* Decode JPEG/PNG bytes to tightly-packed RGBA8 (comp forced to 4).
 * Thread-safe (no shared state). Returns NULL on failure; free with img_free(). */
unsigned char *img_decode_rgba(const unsigned char *buf, int len, int *w, int *h);
void img_free(unsigned char *px);
/* Upload RGBA8 pixels as a 2D texture. MUST run on the GL (main) thread.
 * Returns the texture id, or 0 on failure. */
GLuint img_upload_rgba(const unsigned char *px, int w, int h);
/* Decode + upload in one call (main/GL thread only). */
GLuint img_tex_from_memory(const unsigned char *buf, int len, int *out_w, int *out_h);

#endif /* PLEXPOC_IMG_H */
