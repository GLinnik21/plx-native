// THE DIM FOLD — the held page and the lowest modal dim in ONE pass (`gfx::FrameCache::draw_folded`).
//
// WHAT IT REPLACES. A glass menu stands over a frozen page: every frame the host drew that page as one
// full-screen quad (`fs_tex.frag`) and `draw_scrims` then laid the modal dim over it as a second one
// (`fs_field.frag`, the `udim` span), a full-screen blend of a magnified field times the dim's ink.
// Measured on the television (GPU_ACTIVE, median, a settled Library menu): 8.28M cycles a frame, 2.12M of
// them that second quad. This program draws the first and does the second inside it: the same fetch of
// the field and the same dither, a `mix` in place of a second read-modify-write of the whole screen.
//
// THE DIM IS `fs_field.frag`'s, statement for statement: `ink = field.rgb * u_tint.rgb` and the coverage
// `a = field.a * u_tint.a` (`u_tint` is the dim's cascaded colour, `frame::backdrop::Dim::tint`), the ink
// dithered by the shared prelude and then clamped, because the framebuffer clamps a SOURCE colour to
// [0, 1] before it blends and the two-pass picture therefore blended the clamped one.
//
// COLOUR. The held page is an opaque copy of the frame (`FrameCache`: "opaque, over the app ground"), so
// the two passes were `held` then `mix(held, ink, a)` over it, and one pass writes that `mix` directly.
// Only an opaque `held` makes it exact: with `held.a < 1` the first pass would have blended with what
// the framebuffer held under it, which a single draw cannot know, and the colour would need a divide.
// The caller (`popover::host::page_pass`) folds only on a frame it has a page snapshot for, never on a
// video-plane frame.
//
// ALPHA. The default blend is `(SRC_ALPHA, ONE_MINUS_SRC_ALPHA)` for colour and `(ONE, ONE_MINUS_SRC_ALPHA)`
// for alpha. Two passes leave `a + h.a * (1 - a) + dst.a * (1 - h.a) * (1 - a)` in the alpha channel; one
// pass writing `a + h.a * (1 - a)` leaves the same number for ANY `h.a` and ANY destination, because the
// blend adds `dst.a * (1 - source alpha)` and `1 - (a + h.a * (1 - a)) = (1 - h.a) * (1 - a)`.
//
// THE TWO UVs. `v_cuv` is the held snapshot's texel (`u_uvrect` un-flips the bottom-up copy, as every
// `FrameCache` quad does) and `v_fuv` is the field's, the screen top-down: `vs_img.vert` under
// `PLX_FOLD_UV`. Both arrive as bare varyings (`dither.glsl`, cost rule 4); the third fetch is the
// dither tile's, from `v_dither_nc`. The field is on texture unit 3, the held page on 0, the tile on 2.
precision mediump float;
varying highp vec2 v_cuv;
varying highp vec2 v_fuv;
uniform sampler2D u_tex;
uniform sampler2D u_field;
uniform vec4 u_tint;
void main(){
  vec4 h = texture2D(u_tex, v_cuv);
  vec4 f = texture2D(u_field, v_fuv);
  float a = f.a * u_tint.a;
  vec3 ink = clamp(plx_dither(f.rgb * u_tint.rgb), 0.0, 1.0);
  gl_FragColor = vec4(mix(h.rgb, ink, a), a + h.a * (1.0 - a));
}
