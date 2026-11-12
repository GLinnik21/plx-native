/* Compiled INTO libass-plx. The one place the facade reads a private libass structure: the
 * pinned build installs ass_library.h next to the public headers (ci/build-libass.py) so a test
 * can prove that embedded fonts are not extracted again on every replaced track, and so a font the
 * caller already holds in memory is not copied a second time into the library (issue #1328). */
#include "ass_internal.h"
#include "ass_library.h"
#include <limits.h>
#include <stdlib.h>

size_t plxi_library_fonts(ASS_Library *library)
{
    return library ? library->num_fontdata : 0;
}

int plxi_library_borrow_font(ASS_Library *library, const char *name, const uint8_t *data, size_t len,
                             size_t *index)
{
    static const char placeholder = 0;
    if (!library || !name || !data || !len || len > INT_MAX || !index)
        return -1;
    size_t held = library->num_fontdata;
    /* ass_add_font copies `size` bytes; one placeholder byte costs nothing and leaves a complete
     * entry (strdup'd name, array grown, count bumped) whose data pointer we then replace. An
     * allocation failure inside it returns without counting the entry, which is the -1 here. */
    ass_add_font(library, name, &placeholder, 1);
    if (library->num_fontdata != held + 1)
        return -1;
    ASS_Fontdata *entry = &library->fontdata[held];
    free(entry->data);
    /* libass reads a font's bytes (FT_New_Memory_Face, get_data_embedded) and never writes them. */
    entry->data = (char *)data;
    entry->size = (int)len;
    *index = held;
    return 0;
}

void plxi_library_forget_borrowed(ASS_Library *library, const size_t *indices, size_t count)
{
    if (!library)
        return;
    for (size_t i = 0; i < count; i++) {
        if (indices[i] >= library->num_fontdata)
            continue;
        library->fontdata[indices[i]].data = NULL;
        library->fontdata[indices[i]].size = 0;
    }
}
