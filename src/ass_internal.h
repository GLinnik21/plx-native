#ifndef PLX_ASS_INTERNAL_H
#define PLX_ASS_INTERNAL_H
/* Facade-internal helpers that need libass's private headers (src/ass_fonts.c). Not exported:
 * the library's version script only lets plx_ass_* leave. */
#include <ass/ass.h>
#include <stddef.h>

/* Fonts the library holds (added through ass_add_font, by the facade or by [Fonts] extraction). */
size_t plxi_library_fonts(ASS_Library *library);

#endif
