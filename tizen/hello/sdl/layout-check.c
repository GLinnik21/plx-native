/* Compile-only ABI gate for plx/src/sdl.rs (build-sdl.sh compiles it with each arch's own compiler and
 * never links it, like ci/ffabi-assert.c does for FFmpeg). plx/src/sdl.rs hard-codes the leading part
 * of SDL_Surface, SDL_PixelFormat, SDL_Rect, SDL_Color and a few constants, because it binds the C API
 * by hand. The mirror structs below are the C spelling of those Rust `#[repr(C)]` structs, field for
 * field; the asserts prove the mirror is byte-identical to the installed SDL headers on THIS arch
 * (32-bit ARM, x86_64, i686 differ in pointer width and padding). Change sdl.rs and this file together. */
#include <stddef.h>
#include <stdint.h>
#include "SDL.h"

typedef struct { int x, y, w, h; } MirrorRect;
typedef struct { uint8_t r, g, b, a; } MirrorColor;
typedef struct { uint32_t format; } MirrorPixelFormatPrefix;
typedef struct {
    uint32_t flags;
    void *format;
    int w, h, pitch;
    void *pixels;
    void *userdata;
    int locked;
    void *list_blitmap;
    MirrorRect clip_rect;
    void *map;
    int refcount;
} MirrorSurface;

#define SAME(type, mirror, field) _Static_assert(offsetof(type, field) == offsetof(mirror, field), #type "." #field " moved")

_Static_assert(sizeof(MirrorSurface) == sizeof(SDL_Surface), "SDL_Surface size");
SAME(SDL_Surface, MirrorSurface, flags);
SAME(SDL_Surface, MirrorSurface, format);
SAME(SDL_Surface, MirrorSurface, w);
SAME(SDL_Surface, MirrorSurface, h);
SAME(SDL_Surface, MirrorSurface, pitch);
SAME(SDL_Surface, MirrorSurface, pixels);
SAME(SDL_Surface, MirrorSurface, userdata);
SAME(SDL_Surface, MirrorSurface, locked);
SAME(SDL_Surface, MirrorSurface, list_blitmap);
SAME(SDL_Surface, MirrorSurface, clip_rect);
SAME(SDL_Surface, MirrorSurface, map);
SAME(SDL_Surface, MirrorSurface, refcount);

_Static_assert(sizeof(MirrorRect) == sizeof(SDL_Rect), "SDL_Rect size");
_Static_assert(sizeof(MirrorColor) == sizeof(SDL_Color), "SDL_Color size");
SAME(SDL_Color, MirrorColor, r);
SAME(SDL_Color, MirrorColor, g);
SAME(SDL_Color, MirrorColor, b);
SAME(SDL_Color, MirrorColor, a);
SAME(SDL_PixelFormat, MirrorPixelFormatPrefix, format);
_Static_assert(sizeof(SDL_version) == 3, "SDL_version is three bytes");

/* The constants sdl.rs spells as numbers. */
_Static_assert(SDL_INIT_TIMER == 0x00000001u, "SDL_INIT_TIMER");
_Static_assert(SDL_INIT_EVENTS == 0x00004000u, "SDL_INIT_EVENTS");
_Static_assert(SDL_PIXELFORMAT_ARGB8888 == 0x16362004u, "SDL_PIXELFORMAT_ARGB8888");
_Static_assert(SDL_PIXELFORMAT_ABGR8888 == 0x16762004u, "SDL_PIXELFORMAT_ABGR8888");
_Static_assert(SDL_BLENDMODE_BLEND == 1, "SDL_BLENDMODE_BLEND");
_Static_assert(sizeof(SDL_Event) <= 64, "sdl.rs reserves 64 bytes for an SDL_Event");
