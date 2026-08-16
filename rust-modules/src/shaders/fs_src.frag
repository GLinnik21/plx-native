// The SDF rounded-rect FILL: vertical gradient fill + per-corner-side radius (u_radius left /
// u_radR right) + the focus ring/glow, with a flat fast-path for radius-0 non-focus rects
// (full-screen scrims/backdrops pay one mix, no SDF). Also carries the focus edge-sheen
// (u_rimw/u_rimcol, additive like the focus ring) so a rounded FILL can draw the 1px perimeter
// stroke in its own pass - same fold as fs_img.frag, for the skeleton/chip tiles that have no
// texture. Disabled by u_rimcol.a == 0 (the default from draw_rect/draw_rrect).
//
// PRECISION (this file, fs_shadow.frag, fs_img.frag - and the texture UVs in fs_text*.frag): the
// varying + every op that carries pixel COORDINATES must be `highp`. Midgard interpolates mediump
// varyings in fp16, whose error grows with quad size - on card-sized quads it wobbles the SDF
// distance by ~0.1-0.5px along a straight edge, dashing the 1px AA/rim rows into a "ribbed" edge
// (deterministic, worst on the resume bar's full-card quad; verified on-device by pixel-diffing
// captures). Texture coordinates on wide 1:1 quads need it too: fp16 v_cuv is ~1 texel off at the
// right of a 1920px backdrop (GL_LINEAR then blurs/skips texel columns). The color path stays
// mediump, and the stored `d` may drop back to mediump (fp16 is exact near 0; the cancellation
// happens inside the highp sdBox). vy is a deliberate mediump copy so the flat fast-path's mix
// stays fp16.
precision mediump float;
varying highp vec2 v_uv;
uniform highp vec2 u_size;
uniform highp float u_pad;
uniform highp float u_radius;
uniform vec4 u_colTop;
uniform vec4 u_colBot;
uniform float u_focus;
uniform float u_focus_rgb;
uniform highp float u_radR;
uniform float u_rimw;
uniform vec4 u_rimcol;
highp float sdBox(highp vec2 p, highp vec2 b, highp float r){
  highp vec2 q = abs(p) - b + vec2(r);
  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;
}
void main(){
  float vy = v_uv.y;
  if (u_radius < 0.5 && u_radR < 0.5 && u_focus < 0.001) {
    gl_FragColor = mix(u_colTop, u_colBot, vy);
    return;
  }
  highp vec2 p = (v_uv - 0.5) * u_size;
  highp vec2 hsz = u_size * 0.5 - vec2(u_pad);
  // INTERIOR EARLY-OUT. A rounded rect's edge work is only ever needed within a couple of pixels of
  // its border, and a big panel is mostly not that: the Library's Sort popover is 640x700, so ~90%
  // of its ~450K fragments were running sdBox + two smoothsteps to arrive at "solidly inside".
  // Measured there at 7.1ms of a 33ms frame (`menu.panel`, via /tmp/plxnative-profile), which is
  // what made an open menu drop the screen from 60 to ~30 - the frame lands either side of vsync,
  // so it alternates 16/33ms rather than degrading smoothly.
  //
  // The test is the axis-aligned box inset by the corner radius and the rim width, which is
  // CONSERVATIVE: with |p.x| < hsz.x - rad - m and the same in y, sdBox's q is negative on both
  // axes, so d = max(q.x,q.y) - rad < -m. At m >= 2 that puts d below every threshold below -
  // aFill's smoothstep(-1,1,d) is 1, the rim's smoothstep(-rimw-0.75, ...) is 0, and both focus
  // terms are already 0 inside (the ring's (1-smoothstep(1.5,4,|d-5|)) vanishes for d << 0 and the
  // glow carries step(0.0,d)). So this returns exactly what the full path would, and the branch is
  // coherent across a tile, which is what makes it pay on a tiler.
  highp float radm = max(u_radius, u_radR);
  highp vec2 inner = hsz - vec2(radm + u_rimw + 2.0);
  if (abs(p.x) < inner.x && abs(p.y) < inner.y) {
    gl_FragColor = mix(u_colTop, u_colBot, vy);
    return;
  }
  highp float rad = (p.x > 0.0) ? u_radR : u_radius;
  float d = sdBox(p, hsz, rad);
  vec4 fill = mix(u_colTop, u_colBot, vy);
  float aFill = 1.0 - smoothstep(-1.0, 1.0, d);
  vec3 rgb = fill.rgb * aFill;
  float a = aFill * fill.a;
  float rim = smoothstep(-u_rimw - 0.75, -u_rimw + 0.75, d) * (1.0 - smoothstep(-0.5, 0.5, d)) * u_rimcol.a;
  rgb += u_rimcol.rgb * rim;
  a = max(a, rim);
  if (u_focus > 0.001) {
    float ring = (1.0 - smoothstep(1.5, 4.0, abs(d - 5.0))) * u_focus;
    float glow = exp(-max(d, 0.0) / 14.0) * 0.40 * u_focus * step(0.0, d);
    rgb += (vec3(1.0) * ring + vec3(0.85, 0.9, 1.0) * glow) * u_focus_rgb;
    a = max(a, max(ring, glow));
  }
  gl_FragColor = vec4(rgb, a);
}
