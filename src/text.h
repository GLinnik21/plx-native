#ifndef PLEXPOC_TEXT_H
#define PLEXPOC_TEXT_H
/* SDL2_ttf text rendering: per-ptsize regular/bold font cache, a glyph-texture
 * LRU cache keyed by (string,size,bold), and draw_text with alignment. Sits above
 * gfx: compiles its own text program via gfx_compile() and calls gfx_use_base()
 * to restore the resting program after each draw. Implementation in text.c. */

void init_text(void);
/* align: 0 left, 1 center, 2 right (x is the anchor edge). returns text width. */
float draw_text(const char *s, float x, float y, int sz,
                const float col[4], int align, int bold);

#endif /* PLEXPOC_TEXT_H */
