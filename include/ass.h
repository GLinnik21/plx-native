#ifndef PLX_ASS_H
#define PLX_ASS_H

/* Private, versioned facade over the pinned bundled libass. No upstream struct
 * crosses this ABI; every handle and returned pixel buffer belongs to one worker. */
#include <stddef.h>
#include <stdint.h>

typedef struct PlxAss PlxAss;
typedef struct {
    int x, y, width, height;
    size_t bytes;
    const uint8_t *rgba; /* straight RGBA; valid until the next render or destroy */
} PlxAssBitmap;

unsigned plx_ass_abi_version(void);
PlxAss *plx_ass_create(const char *default_font);
void plx_ass_destroy(PlxAss *ctx);
int plx_ass_add_font(PlxAss *ctx, const char *name, const uint8_t *data, size_t len);
int plx_ass_load(PlxAss *ctx, const uint8_t *data, size_t len, int script);
int plx_ass_chunk(PlxAss *ctx, const uint8_t *data, size_t len,
                  int64_t start_ms, int64_t duration_ms);
void plx_ass_prune_before(PlxAss *ctx, int64_t deadline_ms);
void plx_ass_flush_events(PlxAss *ctx);
/* -1: refused/failed, 0: identical output (bitmap unchanged), 1: changed output.
 * A changed bitmap with width=height=0 clears the old subtitle. */
int plx_ass_render(PlxAss *ctx, int64_t now_ms, int width, int height,
                   int storage_width, int storage_height,
                   PlxAssBitmap *bitmap);

#endif
