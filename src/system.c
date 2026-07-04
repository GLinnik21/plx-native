/* system.c — webOS/SDL platform glue (see system.h). */
#define SDL_MAIN_HANDLED
#include <SDL2/SDL.h>
#include <SDL2/SDL_syswm.h>
#include <GLES2/gl2.h>
#include "app.h"
#include "system.h"

/* Wayland: make our GL surface non-opaque so the starfish video plane
 * below shows through. webOS LSM marks app windows opaque by default and
 * ignores buffer alpha; we must explicitly clear the opaque region.
 * The TV's SDL is 2.0.4 (no transparency hint), so we drive the wayland
 * proxy directly. wl_surface opcodes: 4=set_opaque_region, 6=commit. */
extern void wl_proxy_marshal(void *proxy, unsigned opcode, ...);
extern int wl_display_flush(void *display);
static void *g_wl_surface = NULL, *g_wl_display = NULL;

void clear_opaque_region(void) {
    if (!g_wl_surface) return;
    /* set_opaque_region(NULL) only. Surface state is double-buffered and
     * applied on the next commit — let SDL_GL_SwapWindow do that commit
     * (a bare commit here, before SDL attaches a buffer, presents a
     * null-buffer surface and disrupts the slaved video plane). */
    wl_proxy_marshal(g_wl_surface, 4, (void *)0);
}

/* ---- starfish playback via com.webos.media over luna-service2. Minimal extern
 * decls; no LS2/glib headers. These LS2 decls are vestigial (the buffer-feed
 * path does NOT LSRegister its own client) but kept verbatim for reference. ---- */
typedef int (*LSFilterCb)(void *sh, void *msg, void *ctx);
/* LSError layout (luna-service2, 32-bit ARM): int + 4 ptrs + magic */
struct LSErr { int code; char *message; const char *file; int line;
               const char *func; void *pad; unsigned long magic; };
extern int  LSErrorInit(struct LSErr *e);
extern void LSErrorFree(struct LSErr *e);
extern int LSRegister(const char *name, void **sh, struct LSErr *lserror);
extern int LSCall(void *sh, const char *uri, const char *payload,
                  LSFilterCb cb, void *ctx, unsigned long *token,
                  void *lserror);
extern int LSCallOneReply(void *sh, const char *uri, const char *payload,
                          LSFilterCb cb, void *ctx, unsigned long *token,
                          void *lserror);
extern int LSGmainAttach(void *sh, void *mainloop, void *lserror);
extern const char *LSMessageGetPayload(void *msg);
extern int g_main_context_iteration(void *ctx, int may_block);
extern int g_main_context_pending(void *ctx);

void ls2_pump(void) {
    int guard = 8;
    while (guard-- && g_main_context_pending(NULL))
        g_main_context_iteration(NULL, 0);
}

/* grab the wayland surface/display and make it non-opaque */
void sys_grab_wayland(void *winp) {
    SDL_Window *win = (SDL_Window *)winp;
    /* The TV's SDL fork writes a LARGER SDL_SysWMinfo than our SDL_syswm.h
     * declares, so SDL_GetWindowWMInfo overruns a bare `SDL_SysWMinfo wm` and
     * smashes the stack (corrupting the caller's frame — this was the modular-
     * split crash). Over-allocate a generous buffer for it to write into. */
    char wmbuf[512] = {0};
    SDL_SysWMinfo *wm = (SDL_SysWMinfo *)wmbuf;
    SDL_VERSION(&wm->version);
    int a = -1;
    SDL_GL_GetAttribute(SDL_GL_ALPHA_SIZE, &a);
    GLint abits = -1, rbits = -1;
    glGetIntegerv(GL_ALPHA_BITS, &abits);
    glGetIntegerv(GL_RED_BITS, &rbits);
    if (elogf) {
        fprintf(elogf, "FB bits: alpha=%d red=%d (config alpha=%d)\n",
                abits, rbits, a);
        fflush(elogf);
    }
    if (SDL_GetWindowWMInfo(win, wm)) {
        /* the info union's wayland struct is {wl_display*, wl_surface*,
         * wl_shell_surface*}; all union members share offset 0, so read
         * the first two pointers directly (header-version independent) */
        void **p = (void **)&wm->info;
        g_wl_display = p[0];
        g_wl_surface = p[1];
    }
    if (elogf) {
        fprintf(elogf, "wm subsys=%d wl_surface=%p wl_display=%p alpha=%d\n",
                wm->subsystem, g_wl_surface, g_wl_display, a);
        fflush(elogf);
    }
    clear_opaque_region();
}
