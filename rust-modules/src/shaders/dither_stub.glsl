// THE PRELUDE'S STUB — `plx_dither` by the same name, doing nothing, so that ONE fragment source can
// be linked twice: once behind `dither.glsl` (the wash's program) and once behind this (the hero
// scrim's, `gfx::draw_grad4`), and the plain program carries no uniform, no sampler and no fetch.
//
// It exists because the prelude has no off switch of its own: it is straight-line (cost rule 1 in
// `dither.glsl` — a uniform branch measured +5.8M arithmetic words a frame on a scrolling Library,
// 2026-09-19), so a wash that must be undithered is a different program, not a zero uniform. See
// `gfx::glsl_undithered!` and `gfx::ambient_program`.
precision mediump float;
vec3 plx_dither(vec3 c){ return c; }
