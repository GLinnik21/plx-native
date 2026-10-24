// THE TWO-STOP RAMP — a square, unfocused rect whose top and bottom colours differ. `gfx::draw_rect`
// routes it here instead of through `fs_src.frag`'s flat fast path (`if (u_radius < 0.5 && u_radR <
// 0.5 && u_focus < 0.001) { gl_FragColor = mix(u_colTop, u_colBot, vy); }`), and the expression is
// that path's own, character for character, so the pixels are the same ones.
//
// It exists for ONE surface class: the hero's backdrop ramps. Home draws two full-width ramp bands
// (~1.37M fragments a frame) and, through `fs_src.frag`, each of those fragments measured ~3
// arithmetic words (`plxnative-hwcnt`, 2026-10-24) where one `mix` is 1-1.5: the three uniform
// compares ahead of it are paid on every fragment, and Midgard sizes the register file for the
// whole shader, so the SDF, capsule and glow code this path never runs still lowers the
// occupancy it runs at. Here there is nothing but the mix.
//
// UNDITHERED, exactly like the path it replaces: a rect carries no `dither.glsl` prelude (the shared
// dither's uniform branch alone was +4M shader words a frame across the hero paging scene,
// `docs/backdrop-blur-profiling.md`, 2026-09-04), and a unit test holds this file to that. The
// ambient program is the other cheap ramp this renderer has and is NOT a substitute: its colour
// varying is mediump and its dithered twin would change the picture (and routing the hero's ramps
// through it measured 0.6M cycles dearer, 2026-09-02).
//
// PRECISION mirrors `fs_src.frag` line for line: a highp `v_uv` (the vertex shader's own), the
// colours at the file default (mediump), and `vy` a deliberate mediump copy so the mix stays fp16.
//
// The vertex half is `vs_src.vert`, so `u_rect`/`u_screen` mean what they mean everywhere else.
precision mediump float;
varying highp vec2 v_uv;
uniform vec4 u_colTop;
uniform vec4 u_colBot;
void main(){
  float vy = v_uv.y;
  gl_FragColor = mix(u_colTop, u_colBot, vy);
}
