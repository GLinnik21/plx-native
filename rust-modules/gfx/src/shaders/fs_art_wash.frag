// THE WASH WITH ITS PHOTOGRAPH DISSOLVED INTO IT, in one opaque pass: what `fs_ambient.frag` and
// then `fs_img.frag`'s plain path blended over it produce, for the pixels where the two overlap.
//
// Home's snap dive fades the hero photograph by (1 - snap) while sliding it up, and Detail's scroll
// fades its still the same way, so for the whole of either motion the panel under the art is the
// opaque wash AND the translucent photograph — two full passes over the same pixels. On the dev
// television (Mali-T820, 2026-09-28) that second pass is the frame's excess: the poster dive ran
// 17-24 ms a frame across its whole curve, and drawing the wash only where the art is NOT took it
// to 59.9 moving fps with no frame over 18 ms (`gfx::draw_art_wash` has the numbers). The frame is
// arithmetic-bound, so the pass has to go, not get cheaper: here the wash is evaluated where the
// art already is, and one fragment does the work of two.
//
// The wash is opaque, so the layered result — wash, then the art, then the screen's atmospheric
// ramp of scrim ink over both (`gfx::draw_ambient_inked` has the ramp's half) — is exactly
//
//   dst' = mix(mix(wash, art * tint.rgb, art.a * tint.a), ink, ramp)
//
// and the wash takes its noise BEFORE the photograph is laid over it, as it did as a layer. What
// differs is 8-bit rounding: one quantisation where the layers made three.
//
// Pairs with vs_ambient.vert built with PLX_ART_WASH (and PLX_DITHER_NC): v_col IS the wash's
// per-vertex field over the same mesh, and v_cuv the art quad's own texture coordinate.
precision mediump float;
varying vec4 v_col;
varying highp vec2 v_cuv;
uniform sampler2D u_tex;
uniform vec4 u_tint;   // the art's tint; alpha already carries the painter's cascade
varying float v_inka;  // the ink ramp's alpha here (0 where the screen has no ramp)
uniform vec3 u_ink;    // the ramp's ink, carrying the painter's rgb gain
void main(){
  vec4 c = texture2D(u_tex, v_cuv);
  vec3 g = mix(plx_dither(v_col.rgb), c.rgb * u_tint.rgb, c.a * u_tint.a);
  gl_FragColor = vec4(mix(g, u_ink, v_inka), 1.0);
}
