// The band's card-local coordinate and gradient are affine: compute at four vertices and
// interpolate, instead of reconstructing and dividing on every fragment of every still.
attribute vec2 a_pos;
uniform vec4 u_rect;
uniform vec2 u_screen;
uniform highp vec4 u_size; // full card size, half-size minus radius
uniform highp vec4 u_band; // gradient height, radius, fully covered central half-extents
varying highp vec2 v_p;
varying mediump float v_ramp;
void main(){
  const highp float bleed = 1.0; // gfx::AA_BLEED
  highp vec2 local = a_pos * vec2(u_size.x + 2.0 * bleed, u_band.x + 2.0 * bleed)
      + vec2(-bleed, u_size.y - u_band.x - bleed);
  v_p = local - u_size.xy * 0.5;
  v_ramp = (a_pos.y * (u_band.x + 2.0 * bleed) - bleed) / u_band.x;
  vec2 ndc = (u_rect.xy + a_pos * u_rect.zw) / u_screen * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
