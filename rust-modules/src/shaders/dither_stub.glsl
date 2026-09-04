// THE PRELUDE'S STUB — `plx_dither` by the same name, doing nothing, so that ONE fragment source can
// be linked twice: once behind `dither.glsl` (the at-rest program) and once behind this (the
// in-flight one), and the in-flight program carries no uniform, no sampler and no branch at all.
//
// It exists because a uniform branch is cheap and not free: `u_dither = 0` skips the fetch and the
// add, but on a 2.07M-fragment wash drawn under every Home fold and every cast-row scroll the
// branch alone is on the order of a million GPU cycles a frame for noise that is off. See
// `gfx::glsl_undithered!` and `gfx::ambient_program`.
precision mediump float;
vec3 plx_dither(vec3 c){ return c; }
