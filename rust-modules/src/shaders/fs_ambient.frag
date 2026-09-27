// 4-corner bilinear gradient. TWO roles share this program because they are one field: the opaque
// full-screen ambient wash (`gfx::draw_ambient`, which forces every corner's alpha to 1.0) and the
// alpha-carrying corner gradient that sits OVER artwork (`gfx::draw_grad4` — the hero text scrim).
// Alpha is interpolated with the colour, so the four corners must share an rgb: straight
// (non-premultiplied) rgba only interpolates exactly when they do. One ink at four alphas, never
// four hues at four alphas.
//
// NO COLOUR ARITHMETIC PER FRAGMENT. The field is evaluated per vertex of `gfx::field_mesh` by
// `vs_ambient.vert` and arrives as one interpolated colour. The three-mix form of this shader
// priced the hero's corner scrim at 3.2M GPU cycles a frame on the set (2026-09-02); the one-mix,
// three-varying form that followed cost the full-screen wash ~0.4M more than this (2026-09-19).
//
// PRECISION: colour-only, no edges, no texture, so the colour varying is mediump (fp16), as the
// coordinate of the quad form was. A highp coordinate was tried here (2026-09-01, against contours seen on a slow wash) and
// it is what the dither below actually addresses, not the interpolation: an fp16 uv steps by about
// 1/1000 of the quad, i.e. a colour error far under one 8-bit quantum across even a 1920px span —
// while a highp coordinate promotes the three mixes to fp32 and, measured on the television
// (the HWCNT vinstr profiler, 2026-09-02), that alone priced the hero's corner scrim at ~4.5 arithmetic
// words a fragment, 3.2M GPU cycles of a 11.7M-cycle frame. Banding on an opaque ground is an
// OUTPUT-quantisation problem and the noise is its cure; the varying was never the cause.
//
// DITHER: framebuffer GL_DITHER is intentionally disabled globally because its ordered dot pattern
// damaged shadows and rounded edges. An opaque ambient field still needs unstructured noise or its
// deliberately slow gradient bands. `draw_ambient` dithers the wash on EVERY frame, moving or not
// (since 2026-09-19 — the gates that were tried are recorded there). Only `draw_grad4`, whose alpha
// gradient is a scrim over ARTWORK rather than a ground the eye rests on (dithering it would be
// adding grain to a photograph), takes this same source behind `shaders/dither_stub.glsl`: a twin
// program with no uniform and no fetch at all (`gfx::ambient_program`, 2026-09-04).
//
// **This shader's dither is `dither.glsl`'s now** (2026-09-02). It was written here first and every
// other slow gradient in the app either had a worse answer or none — `fs_glass.frag`, the popover
// background, had a `fract(sin(dot(…)))` hash running UNCONDITIONALLY, which is the exact mistake
// this file's own COST note records having made and fixed. The whole of the reasoning, the three
// measured cost rules and the tile's size argument moved to that prelude; nothing about the picture
// this program produces changed, and `gfx.rs`'s shader test still pins the divisor to `NOISE_DIM`.
precision mediump float;
varying vec4 v_col;
void main(){
  gl_FragColor = vec4(plx_dither(v_col.rgb), v_col.a);
}
