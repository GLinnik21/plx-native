// Card-composite vertex shader (pairs with fs_img.frag - and with fs_blur.frag, which reads only
// v_cuv). Hoists the two per-fragment affine terms to interpolated varyings (correct - both are
// affine in a_pos): v_cuv = the texture UV the quad samples, v_p = card-local pixel coords for
// the SDF.
//
// u_uvrect is (offset.xy, scale.zw) - the sub-rect of the SOURCE this quad maps to, as one mad.
// It was a bare `u_uvscale` centred on 0.5 (quad/card size, for the shadow inflation), which is
// the special case offset = 0.5 - 0.5*scale and is CPU-folded as such. It carries an offset now
// because `gfx::draw_blur_backdrop` samples an arbitrary screen-space window of the blur snapshot,
// and a scale about the centre cannot express one. A NEGATIVE scale.w is legal and load-bearing:
// that is how a bottom-up render target (every FBO chain here) is sampled the right way up.
//
// PLX_FOCUS (linked as its own program - see fs_img.frag's own PLX_FOCUS note): v_gloss hoists the
// GLOSS projection here because it is AFFINE in `a_pos` (a `linear-gradient` scalar), same
// reasoning as v_cuv/v_p - computed once per vertex and interpolated for free by the rasterizer
// instead of every fragment paying two `mad`s and a multiply. This is why the extra uniforms and
// varying live behind the SAME macro as fs_img.frag's focus code, rather than always being
// declared: every draw through the plain (non-`PLX_FOCUS`) program - every resting card, glyph,
// blur reduction, `field_kick`, `FrameCache` quad - stays the exact smaller program main shipped
// before this feature, with no v_gloss load and no extra uniform to fetch, which is the entire
// point of splitting the program in two. `u_focus.z` is the risen shadow's downward shift `dy` (0
// unless this card is focused) - the CARD's own coordinate is `v_p.y + u_focus.z`, never raw
// `v_p.y`, matching the fragment shader's own `vp` correction exactly, so the gloss reads from the
// card's TRUE centre even though the quad's own centre (what `v_p` is relative to) sits `dy` px
// above it once the shadow's asymmetric padding is in play (see gfx.rs's `draw_tex_impl`).
attribute vec2 a_pos;
uniform vec4 u_trect;
uniform vec2 u_tscreen;
uniform vec4 u_uvrect;
#ifdef PLX_FOCUS
uniform highp vec4 u_card; // half-size minus radius, radius, conservative interior threshold
uniform highp vec3 u_focus; // (pop factor f, 1/gloss-gradient-length, shadow y-offset px)
#endif
varying vec2 v_cuv;
varying vec2 v_p;
#ifdef PLX_FOCUS
varying highp float v_gloss;
#endif
#ifdef PLX_DITHER_NC
// The dither tile's coordinate (`shaders/dither.glsl`, cost rule 4): target px / NOISE_DIM, linear
// in position, so interpolated exactly and never computed per fragment. Only the programs whose
// vertex source is built with `gfx::glsl_vs_dithered!` carry it — never the poster/card path.
varying highp vec2 v_dither_nc;
#endif
#ifdef PLX_STILL_GROUND
uniform highp vec2 u_still_band; // inverse band height, card-local band start
varying mediump float v_still_ramp;
#endif
void main(){
  v_cuv = u_uvrect.xy + a_pos * u_uvrect.zw;
  v_p = (a_pos - 0.5) * u_trect.zw;
#ifdef PLX_FOCUS
  // Same 160deg CSS direction / literals as fs_img.frag's GLOSS term (pinned together by
  // `image_focus_geometry_matches_the_shader_literals`); `u_focus.x <= 0` (every draw but at most
  // one focused card) still costs one mad + one multiply here, which the fragment shader would
  // have paid PER FRAGMENT instead - this is the whole point of moving it.
  highp float vpy = v_p.y + u_focus.z;
  highp float chw = u_card.x + u_card.z;
  highp float chh = u_card.y + u_card.z;
  v_gloss = ((v_p.x + chw) * 0.34202014 + (vpy + chh) * 0.93969262) * u_focus.y;
#endif
#ifdef PLX_STILL_GROUND
  v_still_ramp = (v_p.y - u_still_band.y) * u_still_band.x;
#endif
  vec2 px = u_trect.xy + a_pos * u_trect.zw;
#ifdef PLX_DITHER_NC
  v_dither_nc = px * (1.0 / 256.0);
#endif
  vec2 ndc = px / u_tscreen * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
