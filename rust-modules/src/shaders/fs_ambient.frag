// 4-corner bilinear gradient. TWO roles share this program because they are one field: the opaque
// full-screen ambient wash (`gfx::draw_ambient`, which forces every corner's alpha to 1.0) and the
// alpha-carrying corner gradient that sits OVER artwork (`gfx::draw_grad4` — the hero text scrim).
// Alpha is interpolated with the colour, so the four corners must share an rgb: straight
// (non-premultiplied) rgba only interpolates exactly when they do. One ink at four alphas, never
// four hues at four alphas.
//
// PRECISION: this field routinely spans all 1920 pixels. On the target's Midgard fragment path a
// mediump varying is fp16; its interpolation plateaus then jumps across a quad this large, turning
// a quiet wash into diagonal/blocky contours. Keep the coordinate highp even though the colour can
// remain mediump.
//
// DITHER: framebuffer GL_DITHER is intentionally disabled globally because its ordered dot pattern
// damaged shadows and rounded edges. An opaque ambient field still needs ±half an 8-bit quantum of
// unstructured noise or its deliberately slow gradient bands. `draw_ambient` enables that one-code
// dither; `draw_grad4` sets u_noise to zero because its alpha gradient is a scrim, not a ground.
precision mediump float;
varying highp vec2 v_uv;
uniform vec4 u_atl, u_atr, u_abr, u_abl;
uniform float u_noise;
float hash(highp vec2 p){ return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453); }
void main(){
  vec4 top = mix(u_atl, u_atr, v_uv.x);
  vec4 bot = mix(u_abl, u_abr, v_uv.x);
  vec4 outc = mix(top, bot, v_uv.y);
  outc.rgb += (hash(gl_FragCoord.xy) - 0.5) * u_noise;
  gl_FragColor = outc;
}
