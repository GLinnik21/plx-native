// Soft drop-shadow: an analytic SDF penumbra (one smoothstep, no gaussian/FBO). The quad is the
// shadow box inflated by u_blur on every side; hsz shrinks the solid core back to the box size so
// the blur band falls off OUTWARD over u_blur px. Its own program so the hot fill shader
// (fs_src.frag) pays nothing. Used for the lifted-card focus shadow (ui::press replaced the old
// glow ring with soft-shadow + sheen). Circle = radius w/2. u_off is the occluder (tile) offset
// above the shadow box; the cut below skips the covered interior.
//
// TWO cut shapes, chosen by u_cut, because the occluder can be opaque or translucent:
//   u_cut < 0  — the OPAQUE occluder's cut: a cheap axis-aligned box inset by u_radius+1, which
//                stops short of the corner arcs so the penumbra still rounds them. Everything it
//                leaves inside the occluder is hidden by the occluder itself.
//   u_cut >= 0 — the TRANSLUCENT occluder's cut: the occluder's own rounded rect, corner radius
//                u_cut, so NO ink lands under the panel at all. The box cut is wrong here — the
//                band it leaves between the inset and the edge is at full strength and ends in a
//                hard step, which is a drawn FRAME once you can see through the thing on top of it.
// Coordinate chain is highp - see the PRECISION note in fs_src.frag.
precision mediump float;
varying highp vec2 v_uv;
uniform highp vec2 u_size;
uniform highp float u_radius;
uniform highp float u_blur;
uniform highp float u_off;
uniform highp float u_cut;
uniform vec4 u_col;
highp float sdBox(highp vec2 p, highp vec2 b, highp float r){ highp vec2 q=abs(p)-b+vec2(r); return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r; }
void main(){
  highp vec2 p = (v_uv - 0.5) * u_size;
  highp vec2 hsz = max(u_size*0.5 - vec2(u_blur), vec2(0.0));
  highp vec2 pq = p - vec2(0.0, -u_off);
  if (u_cut >= 0.0) {
    if (sdBox(pq, hsz, min(u_cut, min(hsz.x, hsz.y))) < 0.0) discard;
  } else if (all(lessThan(abs(pq), hsz - vec2(u_radius + 1.0)))) {
    discard;
  }
  float d = sdBox(p, hsz, min(u_radius, min(hsz.x, hsz.y)));
  float a = (1.0 - smoothstep(-u_blur, u_blur, d)) * u_col.a;
  gl_FragColor = vec4(u_col.rgb, a);
}
