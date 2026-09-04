// THE SHARED OUTPUT DITHER — prepended to every program that writes a SLOW GRADIENT over a BROAD
// area (`gfx::glsl_dithered!`). One tile, one uniform pair, one policy, one cost discipline.
//
// WHY IT IS SHARED. An 8-bit framebuffer quantises, and a gradient whose whole range is a handful
// of codes therefore comes out as a staircase of flat plateaus rather than a ramp. Measured on the
// television 2026-09-02, Settings over Home: a column through the modal ground spans luma 55.7 to
// 59.1 over 700 rows and contains FOUR distinct levels, in treads of 158, 157 and 146 rows. That is
// not subtle — it is three horizontal lines across the picture. `fs_ambient.frag` solved this for
// the page wash in 2026-09, at some cost, and every other slow gradient in the app kept its own
// answer or none: `fs_src.frag`'s vertical fill had nothing, `fs_modal_ground.frag` had nothing,
// `fs_shadow.frag`'s penumbra had nothing, and `fs_glass.frag` — the popover background this was
// reported against — had a hash that was both structured AND unconditional. Five programs, four
// answers. This file is the one answer — for the three programs whose ramp is a slow FIELD:
// `fs_ambient.frag`, `fs_modal_ground.frag`, `fs_glass.frag`. `fs_src.frag` and `fs_shadow.frag`
// carried it too for two days and were taken back off on 2026-09-04: rule 1 below makes the branch
// cheap, not free, and on the two programs behind every rect and every card shadow it measured
// +4M shader words a frame on hero paging with every ramp test answering 0 — the whole of a 57→50
// fps regression. A rect's two-stop fill crosses tens of codes over hundreds of pixels; nobody has
// seen a tread on one. `gfx::glsl_dithered!`'s doc and its shader test pin both lists.
//
// IT IS NOT A PRECISION PROBLEM, and reaching for `highp` is the expensive wrong turn. An fp16
// interpolant across a 1920px quad steps by about 1/1000 of the quad, i.e. a colour error far under
// one 8-bit quantum; promoting a mix to fp32 changes nothing you can see and, measured with
// `plxnative-hwcnt`, priced the hero's corner scrim at ~4.5 arithmetic words a fragment — 3.2M
// cycles of an 11.7M-cycle frame. Banding is an OUTPUT-QUANTISATION problem. The cure is noise at
// the output, not more bits in the middle. (`fs_src.frag`'s own PRECISION note is about something
// else entirely — pixel COORDINATES feeding an SDF, where fp16 really does dash a 1px edge.)
//
// THE THREE COST RULES, all of them measured and all of them load-bearing:
//
//  1. **Behind a UNIFORM branch.** Midgard resolves a uniform condition per DRAW, not per fragment,
//     so a surface whose caller set `u_dither` to 0 pays no fetch and no add. The first version of
//     the ambient dither ran unconditionally and multiplied by zero: 5.3M of a 13.8M-cycle Home
//     frame, 38% of it, and hero paging fell to 46 fps. The branch is cheap, NOT free — on the two
//     per-rect programs it measured +4M shader words a frame with every draw answering 0
//     (2026-09-04) — which is why only the three slow-field programs carry this prelude at all.
//  2. **A TEXTURE FETCH, never a hash.** `fract(sin(dot(p,k))*43758.5)` is a range reduction plus a
//     polynomial on this part — about 7 arithmetic words. The arithmetic pipe is what binds a broad
//     quad here; the texture pipe beside it is idle. An interleaved-gradient hash was tried as the
//     middle ground and still cost the full-screen wash ~2 cycles a pixel.
//  3. **A 256-square tile, and the 256 was measured.** A tile is a PERIODIC signal and the eye finds
//     periodic structure far below the contrast at which it resolves the grain making it up. At 64
//     the repeat showed on a captured panel as a 30-across plaid (autocorrelation +0.570 at
//     horizontal lag 64 against +0.13 either side). `gfx::NOISE_DIM` is the number and `gfx.rs`'s
//     shader test pins it to the divisor below.
//
// THE NOISE IS TPDF AT ±1 LSB and both halves live in the TILE (`gfx::noise_tex`): the texel stores
// the mean of two independent full-avalanche hashes, so the expression here is exactly the one a
// ±½ LSB uniform dither would use and the whole difference is the amplitude the caller passes.
// Triangular is not less contour than uniform — it is marginally more absolute error — it removes
// noise MODULATION, the slow breathing/blotching that a signal-dependent error variance reads as.
//
// THE TILE IS BOUND ON TEXTURE UNIT 2, permanently, from `gfx::init`. Unit 0 is every program's own
// texture and unit 1 is `fs_glass.frag`'s sharp source, so 2 is the first free one; binding it once
// for the life of the process means no program pays a bind, and `glActiveTexture` never moves off
// unit 0 on the drawing path.
//
// WHO SETS `u_dither` IS A CPU DECISION — `gfx::dither_for_field`, the one policy — because the
// question is one only the caller can answer: is the field broad enough for a plateau to be
// findable. Motion is NOT part of it for a field: a focus spring on Settings must not strip the
// ground's noise (it did, for one day, and the bands flickered in and out with every animation).
// Only the two page washes under moving artwork read the present gate, `gfx::page_wash_dither`.
precision mediump float;
uniform float u_dither;
uniform sampler2D u_dither_tex; // gfx::noise_tex — 256², TPDF, GL_REPEAT + GL_NEAREST, unit 2

// One noise sample for this fragment, in the ±u_dither/2 range. Screen-space 1:1 under GL_NEAREST,
// so the pattern is fixed to the panel and does not swim when a surface slides.
float plx_noise(){
  return texture2D(u_dither_tex, gl_FragCoord.xy * (1.0 / 256.0)).r - 0.5;
}

// Dither a COLOUR. The common case: a ramp between two rgb values, or a graded texture sample.
vec3 plx_dither(vec3 c){
  if (u_dither > 0.0) {
    c += plx_noise() * u_dither;
  }
  return c;
}
