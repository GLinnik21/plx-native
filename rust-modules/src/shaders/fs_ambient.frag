// 4-corner bilinear gradient. TWO roles share this program because they are one field: the opaque
// full-screen ambient wash (`gfx::draw_ambient`, which forces every corner's alpha to 1.0) and the
// alpha-carrying corner gradient that sits OVER artwork (`gfx::draw_grad4` — the hero text scrim).
// Alpha is interpolated with the colour, so the four corners must share an rgb: straight
// (non-premultiplied) rgba only interpolates exactly when they do. One ink at four alphas, never
// four hues at four alphas.
//
// Color-only - no edges, no texture - so mediump v_uv is fine here (a fp16 uv error shifts a smooth
// gradient by far under one 8-bit quantum, and the same holds for the alpha channel).
precision mediump float;
varying vec2 v_uv;
uniform vec4 u_atl, u_atr, u_abr, u_abl;
void main(){
  vec4 top = mix(u_atl, u_atr, v_uv.x);
  vec4 bot = mix(u_abl, u_abr, v_uv.x);
  gl_FragColor = mix(top, bot, v_uv.y);
}
