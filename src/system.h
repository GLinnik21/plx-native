#ifndef PLEXPOC_SYSTEM_H
#define PLEXPOC_SYSTEM_H
/* webOS/SDL platform glue: extract the wl_surface/wl_display from an SDL window
 * and force the surface non-opaque (drive wl_proxy) so the video plane shows
 * through, and pump the glib/luna main context each frame. Implementation in
 * system.c. */

/* LG webOS extension in the TV's SDL fork: soft-hide the Magic Remote cursor
 * exactly like system apps do (system re-shows it on motion). */
extern int SDL_webOSCursorVisibility(int visible);

/* Grab the wayland surface/display from the SDL window (opaque SDL_Window *). */
void sys_grab_wayland(void *win);
/* Clear the UI surface's opaque region so the video plane shows through. */
void clear_opaque_region(void);
/* Pump the glib/luna main context (bounded). */
void ls2_pump(void);

#endif /* PLEXPOC_SYSTEM_H */
