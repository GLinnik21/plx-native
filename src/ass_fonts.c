/* Compiled INTO libass-plx. The one place the facade reads a private libass structure: the
 * pinned build installs ass_library.h next to the public headers (ci/build-libass.py) so a test
 * can prove that embedded fonts are not extracted again on every replaced track. */
#include "ass_internal.h"
#include "ass_library.h"

size_t plxi_library_fonts(ASS_Library *library)
{
    return library ? library->num_fontdata : 0;
}
