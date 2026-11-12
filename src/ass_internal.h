#ifndef PLX_ASS_INTERNAL_H
#define PLX_ASS_INTERNAL_H
/* Facade-internal helpers that need libass's private headers (src/ass_fonts.c). Not exported:
 * the library's version script only lets plx_ass_* leave. */
#include <ass/ass.h>
#include <stddef.h>
#include <stdint.h>

/* Fonts the library holds (added through ass_add_font, by the facade or by [Fonts] extraction). */
size_t plxi_library_fonts(ASS_Library *library);

/* Add one font to the library WITHOUT copying it: the library's entry points at `data`, which the
 * caller keeps alive and unchanged until plxi_library_forget_borrowed has run and the library has
 * been closed. libass's own ass_add_font mallocs and copies; this uses it for the entry (name,
 * bookkeeping, array growth) and swaps its one-byte copy for the caller's bytes. Stores the entry's
 * index in `*index` and returns 0, or returns -1 with nothing added. */
int plxi_library_borrow_font(ASS_Library *library, const char *name, const uint8_t *data, size_t len,
                             size_t *index);

/* Detach the borrowed entries (by the indices plxi_library_borrow_font gave) from the library, so
 * that ass_library_done frees only what libass allocated. Call after the renderer is done: its
 * faces read the bytes until then. */
void plxi_library_forget_borrowed(ASS_Library *library, const size_t *indices, size_t count);

#endif
