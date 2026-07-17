// Rect-fill vertex shader (pairs with fs_src.frag): unit quad -> pixel rect u_rect, flipped-Y NDC.
// v_uv is the unit-quad position; the fragment side derives both the gradient mix and the SDF
// coords from it.
attribute vec2 a_pos;
uniform vec4 u_rect;
uniform vec2 u_screen;
varying vec2 v_uv;
void main(){
  v_uv = a_pos;
  vec2 px = u_rect.xy + a_pos * u_rect.zw;
  vec2 ndc = px / u_screen * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
