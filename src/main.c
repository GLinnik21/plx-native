/* plexpoc — native webOS GLES2 UI proof-of-concept
 * Apple TV-style shelf UI: rounded-corner cards (SDF shader), spring
 * animations, D-pad focus, FPS counter. Links against the TV's own
 * SDL2 (LG webOS port) and GLESv2.
 */
#define SDL_MAIN_HANDLED
#include <SDL2/SDL.h>
#include <SDL2/SDL_syswm.h>
#include <GLES2/gl2.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <unistd.h>
#include <signal.h>
#include <ucontext.h>
#include <pthread.h>
#include "app.h"
#include "gfx.h"
#include "text.h"
#include "system.h"
#include "stream.h"
#include "aq.h"
#include "mkv.h"
#include "img.h"     /* stb_image: decode JPEG/PNG → RGBA, GL texture upload */
#include "pms.h"     /* Plex Media Server library fetch → pms_movies[] */
#include "posters.h" /* async poster/artwork texture store (2 bg workers) */
#include "playback.h" /* Starfish/ACB video playback + buffer-feed + HUD */
#include "ui_home.h"  /* gallery home model + view + navigation */

/* ---- starfish playback via com.webos.media over luna-service2.
 * The jail only allows this app to register on the bus as ITSELF, so we
 * link libluna-service2 (present in the jail) and keep the connection
 * alive for the app's lifetime — the pipeline lives as long as the
 * client connection does. Minimal extern decls; no LS2/glib headers. ---- */
/* Demo / test PMS part URLs live in app.h (config.local.h overrides). At runtime,
 * writing a part URL to /tmp/poc-url overrides either.
 * On returning to the app (background→foreground), rewind the resume point by
 * RESUME_REWIND_NS (app.h) so playback re-enters on already-seen content. */

FILE *elogf = NULL;              /* shared event/diagnostic log (extern in app.h) */
/* crash tracer: log faulting PC + the /proc/self/maps line containing it, so
 * we can tell which library (libplayerAPIs, gstreamer, ours) faulted */
static void crash_handler(int sig, siginfo_t *si, void *uc) {
    unsigned long pc = 0;
    ucontext_t *c = (ucontext_t *)uc;
#if defined(__arm__)
    pc = (unsigned long)c->uc_mcontext.arm_pc;
#endif
    if (elogf) {
        fprintf(elogf, "\n*** SIGNAL %d addr=%p pc=0x%lx\n", sig,
                si ? si->si_addr : 0, pc);
        FILE *m = fopen("/proc/self/maps", "r");
        if (m) {
            char line[256];
            while (fgets(line, sizeof line, m)) {
                unsigned long lo = 0, hi = 0;
                if (sscanf(line, "%lx-%lx", &lo, &hi) == 2 &&
                    pc >= lo && pc < hi) {
                    fprintf(elogf, "in: %s", line);
                    break;
                }
            }
            fclose(m);
        }
        fflush(elogf);
    }
    _exit(3);
}

static void install_crash_tracer(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = crash_handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);    /* UBSan trap-on-error fires SIGILL/SIGTRAP */
    sigaction(SIGTRAP, &sa, NULL);
}

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    elogf = fopen("/tmp/poc-events.log", "w");
    freopen("/tmp/poc-stderr.log", "w", stderr); /* capture abort/assert text */
    install_crash_tracer();
    SDL_SetMainReady();
    SDL_SetHint(SDL_HINT_VIDEO_ALLOW_SCREENSAVER, "0");
    /* request BACK key delivery from the webOS access policy */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    if (SDL_Init(SDL_INIT_VIDEO) != 0) {
        fprintf(stderr, "SDL_Init: %s\n", SDL_GetError());
        return 1;
    }
    fprintf(stderr, "video driver: %s\n", SDL_GetCurrentVideoDriver());

    SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_ES);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 2);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 0);
    /* per-pixel alpha so the video plane (behind the GUI) can show through:
     * force a full 32-bit RGBA config (webOS EGL otherwise hands back an
     * opaque XRGB window buffer the compositor won't alpha-blend) */
    SDL_GL_SetAttribute(SDL_GL_RED_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_GREEN_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_BLUE_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_ALPHA_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_BUFFER_SIZE, 32);
    SDL_Window *win = SDL_CreateWindow("plexpoc", 0, 0, SCR_W, SCR_H,
                                       SDL_WINDOW_OPENGL | SDL_WINDOW_FULLSCREEN);
    if (!win) {
        fprintf(stderr, "CreateWindow: %s\n", SDL_GetError());
        return 1;
    }
    SDL_GLContext ctx = SDL_GL_CreateContext(win);
    if (!ctx) {
        fprintf(stderr, "GL ctx: %s\n", SDL_GetError());
        return 1;
    }
    SDL_GL_SetSwapInterval(1);
    fprintf(stderr, "GL: %s / %s\n", glGetString(GL_RENDERER),
            glGetString(GL_VERSION));

    sys_grab_wayland(win);
    init_gl();
    init_text();
    init_image();          /* iprog: textured poster/logo/backdrop program */

    /* Fetch the Plex movie catalog once at startup (blocking; own 1MB buffer,
     * numeric PMS host so stream.h's inet_aton path is fine). M0: data only —
     * the shelf still draws the placeholder gradient cards below. */
    int nmov = pms_fetch_movies(PMS_HOST, PMS_PORT, PMS_TOKEN, 1);
    if (elogf) {
        fprintf(elogf, "pms: nmovies=%d\n", nmov);
        for (int i = 0; i < nmov && i < 6; i++)
            fprintf(elogf, "pms[%d]: %s (%d) %s part=%s\n", i, pms_movies[i].title,
                    pms_movies[i].year, pms_movies[i].rating, pms_movies[i].part);
        fflush(elogf);
    }
    posters_init(PMS_HOST, PMS_PORT, PMS_TOKEN);   /* spawn poster fetch/decode workers */

    home_init();   /* card colors + focus scale (fr/fc/snapTarget are ui_home globals) */

    Uint32 lastInput = SDL_GetTicks(), lastAuto = 0;
    int autodir = 1;
    Uint32 t0 = SDL_GetTicks(), fpsT = t0;
    int frames = 0, fpsShown = 0;
    int running = 1;

    acb_init();
    int demo = (argc > 1 && strstr(argv[1], "demo") != NULL);
    unsigned heldSym = 0;          /* client-side key repeat (wayland) */
    Uint32 heldSince = 0, lastRep = 0, scrubLast = 0, scrubT = 0;
    int scrubDir = 0;              /* -1/+1 while scrubbing with LEFT/RIGHT held */
    int bgWasPlaying = 0;          /* backgrounded mid-playback → reload on return */
    int bgWasPaused = 0;           /* was paused when backgrounded → re-pause after resume */
    long long bgPos = 0;           /* saved position to resume from (resumePausePending is a global) */
    /* cursor visibility is SYSTEM-owned on webOS: LSM shows it on remote
     * motion and auto-hides it after idle (keycode 0x1e4 notifies us).
     * SDL_ShowCursor(DISABLE) is a one-way trap: once hidden, pointer
     * motion events stop, so nothing can ever re-enable it. Hands off.
     *
     * Instead, arbitrate in software: a D-pad press enters DPAD mode where
     * hover is ignored (button presses physically wobble the remote and
     * spray motion events); only deliberate pointer movement (accumulated
     * distance) returns control to the pointer. */
    int dpadMode = 0, ptrDrag = 0;   /* ptrDrag: dragging the scrubber with the pointer */
    float motAccum = 0, prevMx = -1, prevMy = -1;
    Uint32 lastPtrMotion = 0; int curHidden = 0;   /* auto-hide the pointer when idle in playback */
    int playing = 0;

    while (running) {
        ls2_pump();
        SDL_Event e;
        while (SDL_PollEvent(&e)) {
            if (elogf && (e.type == SDL_KEYDOWN || e.type == SDL_KEYUP)) {
                const unsigned char *raw = (const unsigned char *)&e;
                fprintf(elogf, "[%u] key type=0x%x sym=0x%x scan=0x%x raw=",
                        SDL_GetTicks(), e.type, e.key.keysym.sym,
                        e.key.keysym.scancode);
                for (int bi = 0; bi < 32; bi++) fprintf(elogf, "%02x", raw[bi]);
                fprintf(elogf, "\n");
                fflush(elogf);
            }
            if (e.type == SDL_QUIT) running = 0;
            /* ---- app background/foreground (LG SDL: SDL_APP_* 0x103..0x106) ----
             * Verified on-device: switching to a full-screen app fires 0x103, the
             * media server releases our pipeline; returning fires 0x105/0x106. */
            else if (e.type == 0x103 || e.type == 0x104) {   /* WILL/DID ENTER BACKGROUND */
                if (elogf) { fprintf(elogf, "LIFECYCLE: background (playing=%d)\n", playing); fflush(elogf); }
                if (playing && !bgWasPlaying) {   /* tear down: system will release the pipeline */
                    bgPos = g_playpos_ns; bgWasPlaying = 1; bgWasPaused = pl_paused;
                    /* a held D-pad scrub / pointer drag would otherwise commit a stale
                     * seek (pl_scrub_ns==-1) on the trailing key-up after resume and
                     * clobber the accurate resume seek — cancel it now */
                    scrubDir = 0; ptrDrag = 0; pl_scrub_ns = -1;
                    stop_bufferfeed(1); playing = 0;   /* keep cues → accurate resume seek */
                }
            }
            else if (e.type == 0x105 || e.type == 0x106) {   /* WILL/DID ENTER FOREGROUND */
                if (elogf) { fprintf(elogf, "LIFECYCLE: foreground (wasPlaying=%d)\n", bgWasPlaying); fflush(elogf); }
                if (bgWasPlaying && e.type == 0x106) {        /* reload + resume on DID-enter */
                    playing = start_bufferfeed();
                    if (playing) {
                        /* Resume rewind: back up a few seconds so returning to the app
                         * re-enters on already-seen content and re-establishes context,
                         * instead of landing at a spot that feels like a jump. Only when
                         * we were playing — a deliberate pause keeps its exact frame. */
                        long long rt = bgPos;
                        if (!bgWasPaused) { rt -= RESUME_REWIND_NS; if (rt < 0) rt = 0; }
                        g_seek_to_ns = rt;
                        pl_hud_until = SDL_GetTicks() + 4500;
                        resumePausePending = bgWasPaused;
                    }
                    bgWasPlaying = 0;
                }
            }
            else if (e.type == SDL_KEYDOWN || e.type == SDL_KEYUP) {
                /* LG's SDL fork inserts an extra 32-bit field after
                 * windowID, shifting SDL_KeyboardEvent: read the real
                 * fields at their actual offsets.
                 *   +16 state (u32), +20 scancode (u32), +24 sym (u32) */
                const unsigned char *raw = (const unsigned char *)&e;
                unsigned state, wcode, sym;
                memcpy(&state, raw + 16, 4);
                memcpy(&wcode, raw + 20, 4);
                memcpy(&sym, raw + 24, 4);
                /* raw state: low byte = pressed(1)/released(0), 0x100 bit = auto-repeat */
                int isnav = (sym == (unsigned)SDLK_LEFT || sym == (unsigned)SDLK_RIGHT ||
                             sym == 417 || wcode == 417 || sym == 412 || wcode == 412);
                if ((state & 0xff) != 1) {   /* real key-up → commit the scrub as a seek */
                    if (sym == heldSym) heldSym = 0;
                    if (playing && scrubDir != 0 && isnav) {
                        g_seek_to_ns = pl_scrub_ns; pl_scrub_ns = -1; scrubDir = 0; scrubT = 0;
                    }
                    continue;
                }
                if (state & 0x100) {         /* auto-repeat: key still held → keep scrub alive */
                    if (playing && scrubDir != 0 && isnav) scrubLast = SDL_GetTicks();
                    continue;                /* don't re-fire first-press handlers */
                }
                lastInput = SDL_GetTicks();
                if (!playing &&
                    (sym == (unsigned)SDLK_LEFT || sym == (unsigned)SDLK_RIGHT ||
                     sym == (unsigned)SDLK_UP || sym == (unsigned)SDLK_DOWN)) {
                    if (!dpadMode) SDL_webOSCursorVisibility(0);
                    dpadMode = 1;
                    motAccum = 0;
                    if (snapTarget < 0.5f) {
                        /* hero: DOWN drops into the grid; UP/LEFT/RIGHT stay on the hero */
                        if (sym == (unsigned)SDLK_DOWN) { snapTarget = 1.0f; fr = 0; }
                    } else if (sym == (unsigned)SDLK_UP && fr == 0) {
                        snapTarget = 0.0f;              /* grid top row → back up to the hero */
                    } else {
                        home_move_focus(sym);           /* navigate within the grid */
                    }
                    heldSym = sym;
                    heldSince = lastInput;
                    lastRep = lastInput;
                }
                else if (wcode == 0x1e4) /* LG: pointer auto-hidden; ignore */
                    ;
                else if (sym == (unsigned)SDLK_RETURN ||
                         sym == (unsigned)SDLK_KP_ENTER ||
                         sym == (unsigned)SDLK_SELECT) {
                    if (!playing) {
                        /* select: hero Play (snap<0.5) plays the hero item; grid plays the focused card */
                        play_movie(snapTarget < 0.5f ? movie_at(0, 0) : movie_at(fr, fc));
                        playing = start_bufferfeed();
                        pl_paused = 0;
                        pl_hud_until = lastInput + 4500;
                        if (!dpadMode) { SDL_webOSCursorVisibility(0); dpadMode = 1; }
                    } else {
                        /* OK during playback → toggle play/pause */
                        pl_paused = !pl_paused;
                        if (pl_paused) playback_pause(); else playback_resume();
                        pl_hud_until = lastInput + 4500;
                    }
                }
                /* dedicated Magic Remote play/pause button (this remote sends the
                 * state-appropriate key: PAUSE=wcode 72 while playing, PLAY=wcode 450
                 * while paused/stopped). Verified from the raw key log. */
                else if (wcode == 72 || sym == 415 || wcode == 415) {          /* PAUSE */
                    if (playing && !pl_paused) { pl_paused = 1; playback_pause(); }
                    pl_hud_until = lastInput + 4500;
                }
                else if (wcode == 450 || sym == 19 || wcode == 19 ||
                         sym == 402 || wcode == 402) {                          /* PLAY */
                    if (!playing) {
                        playing = start_bufferfeed(); pl_paused = 0;
                        if (!dpadMode) { SDL_webOSCursorVisibility(0); dpadMode = 1; }
                    } else if (pl_paused) { pl_paused = 0; playback_resume(); }
                    pl_hud_until = lastInput + 4500;
                }
                else if (playing && (sym == 413 || wcode == 413)) {   /* Stop key */
                    stop_bufferfeed(0); playing = 0;
                }
                else if (playing &&
                         (sym == (unsigned)SDLK_LEFT || sym == (unsigned)SDLK_RIGHT ||
                          sym == (unsigned)SDLK_UP || sym == (unsigned)SDLK_DOWN ||
                          sym == 417 || wcode == 417 || sym == 412 || wcode == 412)) {
                    pl_hud_until = lastInput + 4500;
                    if (!curHidden) { SDL_webOSCursorVisibility(0); curHidden = 1; }  /* D-pad hides pointer */
                    if (ptrDrag) { ptrDrag = 0; pl_scrub_ns = -1; }   /* D-pad cancels a pointer drag */
                    int fwd  = (sym == (unsigned)SDLK_RIGHT || sym == 417 || wcode == 417);
                    int back = (sym == (unsigned)SDLK_LEFT  || sym == 412 || wcode == 412);
                    if ((fwd || back) && pl_dur_ns > 0) {
                        /* start a scrub PREVIEW; the main loop advances it at a
                         * steady rate while held and commits when presses stop. */
                        if (pl_scrub_ns < 0) pl_scrub_ns = g_playpos_ns;
                        pl_scrub_ns += (fwd ? 10LL : -10LL) * 1000000000LL;  /* a tap = ±10s */
                        long long cap = pl_dur_ns - 3LL * 1000000000LL;
                        if (pl_scrub_ns < 0) pl_scrub_ns = 0;
                        if (cap > 0 && pl_scrub_ns > cap) pl_scrub_ns = cap;
                        scrubDir = fwd ? 1 : -1;
                        scrubLast = lastInput;
                    }
                }
                else if (sym == (unsigned)SDLK_ESCAPE || sym == 'q' ||
                         wcode == 461 /* webOS BACK */) {
                    if (playing) { stop_bufferfeed(0); playing = 0; }
                    else if (snapTarget > 0.5f) snapTarget = 0.0f;   /* grid → hero */
                    else running = 0;                                 /* hero → quit */
                }
            }
            else if (e.type == SDL_MOUSEMOTION) {
                /* Magic Remote pointer: hover focuses the card under it */
                lastInput = SDL_GetTicks();
                lastPtrMotion = lastInput; curHidden = 0;   /* pointer moved → it's showing */
                float mx = (float)e.motion.x, my = (float)e.motion.y;
                if (prevMx >= 0)
                    motAccum += fabsf(mx - prevMx) + fabsf(my - prevMy);
                prevMx = mx; prevMy = my;
                if (playing) {              /* pointer wakes HUD; drag updates the scrub */
                    pl_hud_until = lastInput + 4500;
                    if (ptrDrag && pl_dur_ns > 0) {
                        float sbx = 90, sbw = (float)SCR_W - 180;
                        double frac = (mx - sbx) / sbw;
                        if (frac < 0) frac = 0; if (frac > 1) frac = 1;
                        pl_scrub_ns = (long long)(frac * (double)pl_dur_ns);
                        scrubLast = lastInput;
                    }
                    continue;
                }
                if (dpadMode) {
                    /* ignore wobble; a deliberate wave re-engages pointer */
                    if (motAccum < 120.0f) continue;
                    dpadMode = 0;
                }
                home_pointer_focus(mx, my);
            }
            else if (e.type == SDL_MOUSEBUTTONDOWN) {
                /* Magic Remote center-click (arrives as a mouse click when the
                 * pointer is active): on the scrubber → seek; elsewhere → play/pause
                 * (so the center button still works while the pointer is showing). */
                lastInput = SDL_GetTicks();
                if (playing) {
                    float cx = (float)e.button.x, cy = (float)e.button.y;
                    float sbx = 90, sbw = (float)SCR_W - 180;
                    int on_scrub = (pl_dur_ns > 0 && cy > SCR_H - 270 && cy < SCR_H - 110 &&
                                    cx >= sbx && cx <= sbx + sbw);
                    if (on_scrub) {              /* start a drag; commit on button-up */
                        double frac = (cx - sbx) / sbw;
                        if (frac < 0) frac = 0; if (frac > 1) frac = 1;
                        long long t = (long long)(frac * (double)pl_dur_ns);
                        long long cap = pl_dur_ns - 3LL * 1000000000LL;
                        if (cap > 0 && t > cap) t = cap;
                        pl_scrub_ns = t; ptrDrag = 1; scrubLast = lastInput;
                    } else {                       /* toggle play/pause */
                        pl_paused = !pl_paused;
                        if (pl_paused) playback_pause(); else playback_resume();
                    }
                    pl_hud_until = lastInput + 4500;
                }
            }
            else if (e.type == SDL_MOUSEBUTTONUP) {
                /* release a scrubber drag → commit the seek */
                lastInput = SDL_GetTicks();
                if (ptrDrag) {
                    ptrDrag = 0;
                    if (pl_scrub_ns >= 0) { g_seek_to_ns = pl_scrub_ns; pl_scrub_ns = -1; }
                    pl_hud_until = lastInput + 4500;
                }
            }
            else if (e.type == SDL_MOUSEWHEEL) {
                /* wheel = row up/down, Apple TV style (debounced: the
                 * Magic Remote wheel fires bursts of events per notch) */
                static Uint32 lastWheel = 0;
                Uint32 wnow = SDL_GetTicks();
                lastInput = wnow;
                if (wnow - lastWheel > 250) {
                    lastWheel = wnow;
                    home_wheel(e.wheel.y);
                }
            }
        }
        Uint32 now = SDL_GetTicks();
        /* playback is user-driven now: OK on a shelf card calls start_bufferfeed().
         * DEV: /tmp/poc-autoplay auto-presses OK once, for headless screen tests. */
        static int autoTried = 0;
        if (!autoTried && !playing && now - t0 > 2000) {
            autoTried = 1;
            FILE *af = fopen("/tmp/poc-autoplay", "r");
            if (af) { fclose(af);
                      int pidx = 0; FILE *pf = fopen("/tmp/poc-playidx", "r");   /* dev: pick a title */
                      if (pf) { if (fscanf(pf, "%d", &pidx) != 1) pidx = 0; fclose(pf); }
                      play_movie(movie_at(pidx / COLS, pidx % COLS));
                      playing = start_bufferfeed();
                      pl_paused = 0; pl_hud_until = now + 60000; }  /* dev: keep HUD up for capture */
        }
        /* dev: /tmp/poc-grid → start in grid mode (headless snap-state capture) */
        static int gridTried = 0;
        if (!gridTried && now - t0 > 400) {
            gridTried = 1;
            FILE *gf = fopen("/tmp/poc-grid", "r");
            if (gf) { fclose(gf); snapTarget = 1.0f; fr = 0; }
        }
        /* dev: /tmp/poc-autoseek → one auto-seek to 40% at t0+12s (headless test) */
        static int seekTried = 0;
        if (!seekTried && playing && pl_dur_ns > 0 && now - t0 > 12000) {
            FILE *sf = fopen("/tmp/poc-autoseek", "r");
            if (sf) { fclose(sf); g_seek_to_ns = 140LL * 1000000000LL; }  /* dev: seek to 2:20 */
            seekTried = 1;
        }
        if (bf_started) bufferfeed_pump(now);
        /* client-side long-press repeat for the shelf: 400ms delay, then every 130ms */
        if (heldSym && now - heldSince > 400 && now - lastRep > 130) {
            lastRep = now;
            if (snapTarget > 0.5f) home_move_focus(heldSym);   /* hold-to-navigate: grid only */
        }
        /* LEFT/RIGHT scrub: advance the preview at a steady rate while the key is
         * held; commit on key-up (above). The remote's auto-repeat has a ~500ms
         * initial delay, so DON'T commit on a short idle gap — only a long safety
         * fallback (in case a key-up is ever missed). Pointer drag commits on up. */
        if (pl_scrub_ns >= 0 && scrubDir != 0 && !ptrDrag) {
            if (now - scrubLast > 1200) {           /* lost the key-up → commit */
                g_seek_to_ns = pl_scrub_ns; pl_scrub_ns = -1; scrubDir = 0; scrubT = 0;
            } else {                                /* held → ~35s of film per sec */
                float sdt = scrubT ? (now - scrubT) / 1000.0f : 0.016f;
                if (sdt > 0.1f) sdt = 0.1f;
                pl_scrub_ns += (long long)((double)scrubDir * 35.0 * sdt * 1e9);
                long long cap = pl_dur_ns - 3LL * 1000000000LL;
                if (pl_scrub_ns < 0) pl_scrub_ns = 0;
                if (cap > 0 && pl_scrub_ns > cap) pl_scrub_ns = cap;
                pl_hud_until = now + 4500; scrubT = now;
            }
        }
        /* hide the Magic Remote pointer after it's been idle during playback */
        if (playing && !curHidden && !ptrDrag && lastPtrMotion && now - lastPtrMotion > 3000) {
            SDL_webOSCursorVisibility(0);
            curHidden = 1;
        }
        /* re-pause after a resume: keep feeding until the resume seek is consumed and
         * its frame is on screen (a few frames presented), then pause where the user left off */
        if (resumePausePending && playing && !pl_paused && g_seek_to_ns < 0 && bf_frames >= 3 &&
            g_playpos_ns + 15LL * 1000000000LL >= bgPos) {   /* near the resume point, not the play-from-start */
            pl_paused = 1; playback_pause();
            resumePausePending = 0;
        }
        /* auto-demo only when launched with demo param */
        if (demo && now - lastInput > 6000 && now - lastAuto > 900) {
            lastAuto = now;
            fc += autodir;
            if (fc >= COLS) { fc = COLS - 1; autodir = -1; fr = (fr + 1) % ROWS; }
            else if (fc < 0) { fc = 0; autodir = 1; fr = (fr + 1) % ROWS; }
        }
        static Uint32 prev = 0;
        float dt = prev ? (now - prev) / 1000.0f : 0.016f;
        if (dt > 0.05f) dt = 0.05f;
        prev = now;

        home_update(dt);  /* bg phase + focus/scroll/snap springs */

        poster_pump(3);   /* upload up to 3 decoded posters this frame */

        /* ---- draw ---- */
        glViewport(0, 0, SCR_W, SCR_H);
        if (playing) {
            /* Player: keep the graphics plane transparent so the video plane
             * shows through; overlay the transport HUD on interaction. */
            clear_opaque_region();
            glClearColor(0.0f, 0.0f, 0.0f, 0.0f);
            glClear(GL_COLOR_BUFFER_BIT);
            if (now < pl_hud_until || pl_paused) draw_hud();
            SDL_GL_SwapWindow(win);
            frames++;
            if (now - fpsT >= 1000) { frames = 0; fpsT = now; }
            continue;
        }
        home_draw();

        /* FPS counter */
        float fpsCol[4] = {0.4f, 1.0f, 0.55f, 1.0f};
        draw_number(fpsShown, SCR_W - 70, 64, 46, fpsCol);

        SDL_GL_SwapWindow(win);
        frames++;
        if (now - fpsT >= 1000) {
            fpsShown = (int)(frames * 1000.0f / (now - fpsT) + 0.5f);
            printf("FPS %d\n", fpsShown);
            fflush(stdout);
            frames = 0;
            fpsT = now;
        }
    }
    if (bf_started) stop_bufferfeed(0);
    posters_shutdown();
    SDL_Quit();
    return 0;
}
