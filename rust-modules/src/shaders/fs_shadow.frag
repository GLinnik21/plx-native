// Soft drop-shadow: an analytic SDF penumbra (one smoothstep, no gaussian/FBO). The quad is the
// shadow box inflated by u_blur on every side; hsz shrinks the solid core back to the box size so
// the blur band falls off OUTWARD over u_blur px. Its own program so the hot fill shader
// (fs_src.frag) pays nothing. Used for the lifted-card focus shadow (ui::press replaced the old
// glow ring with soft-shadow + sheen). Circle = radius w/2. u_off is the occluder (tile) offset
// above the shadow box; the discard skips the covered interior.
// Coordinate chain is highp - see the PRECISION note in fs_src.frag.
precision mediump float;
varying highp vec2 v_uv;
uniform highp vec2 u_size;
uniform highp float u_radius;
uniform highp float u_blur;
uniform highp float u_off;
uniform vec4 u_col;
highp float sdBox(highp vec2 p, highp vec2 b, highp float r){ highp vec2 q=abs(p)-b+vec2(r); return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r; }
void main(){
  highp vec2 p = (v_uv - 0.5) * u_size;
  highp vec2 hsz = max(u_size*0.5 - vec2(u_blur), vec2(0.0));
  highp vec2 pp = abs(p - vec2(0.0, -u_off));
  if (all(lessThan(pp, hsz - vec2(u_radius + 1.0)))) discard;
  float d = sdBox(p, hsz, min(u_radius, min(hsz.x, hsz.y)));
  float a = (1.0 - smoothstep(-u_blur, u_blur, d)) * u_col.a;
  gl_FragColor = vec4(u_col.rgb, a);
}
