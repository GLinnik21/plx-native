#ifndef PLEXPOC_PLAYBACK_H
#define PLEXPOC_PLAYBACK_H
/* The video-playback subsystem (StarfishMediaAPIs + ACB + buffer-feed + HUD).
 * All Starfish/g_smp/ACB access is confined to playback.c; main drives it
 * through this narrow API plus the shared transport globals below. */
#include "pms.h"      /* pms_movie */

/* ---- controller API (called from main) ---- */
int  acb_init(void);
/* play_movie moved to the Rust route module (rust-modules/src/route.rs) */
int  start_bufferfeed(void);
void stop_bufferfeed(int keep_cues);
void bufferfeed_pump(unsigned now);
/* draw_hud moved to the Rust ui::player_hud module */
void playback_pause(void);    /* guards g_smpReady then SMP_Pause */
void playback_resume(void);   /* guards g_smpReady then SMP_Play */

/* ---- transport state shared with main's controller loop ---- */
extern int                bf_started;
extern int                pl_paused;
extern int                resumePausePending;
extern unsigned           pl_hud_until;
extern long long          pl_scrub_ns;
extern long long          pl_dur_ns;
extern volatile long long g_seek_to_ns;
extern volatile long long g_playpos_ns;
extern volatile int       bf_frames;

#endif /* PLEXPOC_PLAYBACK_H */
