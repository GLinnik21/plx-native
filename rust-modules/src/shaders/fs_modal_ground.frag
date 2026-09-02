// Frozen Settings ground: one sample of the cached blur, then a small wallpaper-style colour
// grade. Saturation is arithmetic, not another texture fetch; the expensive blur was paid once
// when the modal opened.
precision mediump float;
varying highp vec2 v_cuv;
uniform sampler2D u_tex;
uniform vec4 u_tint;
uniform float u_saturation;
void main(){
  vec4 c = texture2D(u_tex, v_cuv);
  float y = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
  vec3 rgb = mix(vec3(y), c.rgb, u_saturation) * u_tint.rgb;
  // The grade is a DESATURATE plus a tint MULTIPLY, and both compress: the blurred source is
  // already an 8-bit near-flat field, and scaling it toward one hue leaves a handful of distinct
  // output codes across the whole screen. Measured on the television before this line existed —
  // Settings over Home, a 700-row column through the ground — luma 55.7 to 59.1 in FOUR levels,
  // treads of 158, 157 and 146 rows. See `dither.glsl`.
  gl_FragColor = vec4(plx_dither(rgb), c.a * u_tint.a);
}
