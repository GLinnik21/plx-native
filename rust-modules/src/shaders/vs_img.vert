// Card-composite vertex shader (pairs with fs_img.frag). Hoists the two per-fragment affine terms
// to interpolated varyings (correct - both are affine in a_pos): v_cuv = the texture UV remapped
// to the inner card sub-rect (u_uvscale = quad/card size, 1.0 when pad==0 so v_cuv == a_pos for
// the flat path), v_p = card-local pixel coords for the SDF.
attribute vec2 a_pos;
uniform vec4 u_trect;
uniform vec2 u_tscreen;
uniform vec2 u_uvscale;
varying vec2 v_cuv;
varying vec2 v_p;
void main(){
  v_cuv = (a_pos - 0.5) * u_uvscale + 0.5;
  v_p = (a_pos - 0.5) * u_trect.zw;
  vec2 px = u_trect.xy + a_pos * u_trect.zw;
  vec2 ndc = px / u_tscreen * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
