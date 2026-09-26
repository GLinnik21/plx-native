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
attribute vec2 a_pos;
uniform vec4 u_trect;
uniform vec2 u_tscreen;
uniform vec4 u_uvrect;
varying vec2 v_cuv;
varying vec2 v_p;
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
