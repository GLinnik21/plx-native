// 4-corner ambient gradient (full-screen backdrop base). Color-only - no edges, no texture - so
// mediump v_uv is fine here (a fp16 uv error shifts a smooth gradient by far under one 8-bit
// quantum).
precision mediump float;
varying vec2 v_uv;
uniform vec4 u_atl, u_atr, u_abr, u_abl;
void main(){
  vec3 top = mix(u_atl.rgb, u_atr.rgb, v_uv.x);
  vec3 bot = mix(u_abl.rgb, u_abr.rgb, v_uv.x);
  gl_FragColor = vec4(mix(top, bot, v_uv.y), 1.0);
}
