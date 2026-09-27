// Artwork's bottom scrim, in ONE band-sized quad. The old corner treatment issued three
// scissored full-card SDF draws in addition to its gradient. Keep this program separate from
// fs_src: the scrim needs none of its focus, rim or capsule registers on the television's GPU.
precision mediump float;
varying highp vec2 v_p;
varying mediump float v_ramp;
uniform highp vec4 u_size; // full card size, half-size minus radius
uniform highp vec4 u_band; // gradient height, radius, fully covered central half-extents
uniform vec4 u_col;
void main(){
  float alpha = u_col.a * clamp(v_ramp, 0.0, 1.0);
  // The central column is fully covered all the way to the bottom AA row. Test the CPU-folded
  // limits before any distance work; only the side strips and AA fringe reach the edge path.
  if (all(lessThan(abs(v_p), u_band.zw))) {
    gl_FragColor = vec4(u_col.rgb, alpha);
    return;
  }
  highp vec2 q = abs(v_p) - u_size.zw;
  // Outside the corner quadrants sdBox reduces exactly to max(q)-radius: no square root
  // across the straight majority of the band, including its antialiased side/bottom edges.
  highp float d = max(q.x, q.y) - u_band.y;
  if (min(q.x, q.y) > 0.0) d = length(q) - u_band.y;
  // Same 2px edge coverage as fs_src/fs_img. Apply coverage only to straight alpha.
  float coverage = 1.0 - smoothstep(-1.0, 1.0, d);
  gl_FragColor = vec4(u_col.rgb, alpha * coverage);
}
