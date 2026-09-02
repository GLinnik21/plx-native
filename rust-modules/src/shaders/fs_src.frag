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
// Extra rim weight on the side FACING THE LIGHT — straight up. 0 disables it and the branch below
// costs nothing. It exists because a container's top edge carries a brighter line than its
// perimeter (the design system's `--glass-rim-light` over `--glass-rim`), and the honest way to
// stop that line is to let it FADE where the surface turns away, not to scissor it: a hard cut
// lands at the widest point of a cap and reads as the outline breaking off mid-air.
uniform float u_rimtop;
// THE CAPSULE OUTLINE — three circular arcs per corner, blended, and NOT a stadium. `w` of the
// second vector is the switch: 0 leaves every capsule in this app exactly the rounded rect it has
// always been. The numbers are solved on the CPU (`ui::pill`, the port of the design project's
// `pillPath.js`) because the big arc's radius comes out of a bisection — 73 000px on a 260x61
// control — and a shader is the wrong place for it.
//   u_pill1 = (big radius R, blend radius f, end radius r, big arc's centre y)
//   u_pill2 = (end circle's centre x, blend centre x, blend centre y, enabled)
// Both centres are in the box's own CENTRED frame, folded into the top-right quadrant, which the
// shape's two axes of symmetry make sufficient.
uniform highp vec4 u_pill1;
uniform highp vec4 u_pill2;
// THE INNER GLOW — `(top depth px, top weight, bottom depth px, bottom weight)`, 0 disables it and
// the branch below costs nothing. The design states the focused control's edge over VIDEO as a
// perimeter line plus two soft inset shadows, a strong one along the top and a .55 one along the
// bottom: light spilling INWARD off the edge, which is what stops a near-white capsule reading as a
// flat chip when the frame behind it is unknown. It is the same lamp as u_rimcol, so it takes that
// colour and only the falloff is its own.
uniform vec4 u_glow;
// Which arc governs a point already folded into that quadrant — `(centre, radius)`. The outline is
// SMOOTH, so at each hand-over the two arcs agree on the distance and on the normal both, and there
// is no seam to place: the test is simply which side of the tangency direction the point falls on,
// and the direction's x is monotone across each arc's own span.
highp vec3 pillArc(highp vec2 q){
  highp vec2 bc = vec2(0.0, u_pill1.w);
  highp vec2 ec = vec2(u_pill2.x, 0.0);
  highp vec2 fc = u_pill2.yz;
  if (normalize(q - bc).x <= normalize(fc - bc).x) return vec3(bc, u_pill1.x);
  // the blend CONTAINS the end circle and touches it from the inside, so the contact sits on the
  // far side of it and the tangency direction points AWAY from the blend's centre
  if (normalize(q - ec).x >= normalize(ec - fc).x) return vec3(ec, u_pill1.z);
  return vec3(fc, u_pill1.y);
}
highp float sdPill(highp vec2 p){
  highp vec2 q = vec2(abs(p.x), -abs(p.y));
  highp vec3 a = pillArc(q);
  return length(q - a.xy) - a.z;
}
highp vec2 pillNormal(highp vec2 p){
  highp vec2 q = vec2(abs(p.x), -abs(p.y));
  highp vec3 a = pillArc(q);
  highp vec2 n = normalize(q - a.xy + vec2(1e-5));
  return vec2(p.x < 0.0 ? -n.x : n.x, p.y > 0.0 ? -n.y : n.y);
}
highp float sdBox(highp vec2 p, highp vec2 b, highp float r){
  highp vec2 q = abs(p) - b + vec2(r);
  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;
}
// The gradient of that field — the outward normal. Two cases, exactly the two terms of sdBox: the
// rounded corner (either component of q positive) and the flat sides. Same construction as
// fs_glass.frag's, and evaluated only on rim fragments of a surface that asked for a top line.
highp vec2 sdBoxNormal(highp vec2 p, highp vec2 b, highp float r){
  highp vec2 sg = sign(p);
  highp vec2 q = abs(p) - b + vec2(r);
  highp vec2 m = max(q, 0.0);
  if (m.x > 0.0 || m.y > 0.0) return sg * normalize(m + vec2(1e-5));
  return sg * (q.x > q.y ? vec2(1.0, 0.0) : vec2(0.0, 1.0));
}
void main(){
  float vy = v_uv.y;
  if (u_radius < 0.5 && u_radR < 0.5 && u_focus < 0.001) {
    // The flat fast path — full-screen scrims and backdrops. Also the broadest quad in the app, so
    // it is the one that bands worst when its two colours differ; see `dither.glsl`.
    vec4 flat_c = mix(u_colTop, u_colBot, vy);
    gl_FragColor = vec4(plx_dither(flat_c.rgb), flat_c.a);
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
    // The interior of a rounded panel — most of its fragments, and the ones a viewer stares at.
    // It has to dither on the SAME terms as the edge path below, or the two exits of this shader
    // disagree and the boundary between them becomes a visible ring.
    vec4 in_c = mix(u_colTop, u_colBot, vy);
    gl_FragColor = vec4(plx_dither(in_c.rgb), in_c.a);
    return;
  }
  highp float rad = (p.x > 0.0) ? u_radR : u_radius;
  bool pill = u_pill2.w > 0.5;
  float d = pill ? sdPill(p) : sdBox(p, hsz, rad);
  vec4 fill = mix(u_colTop, u_colBot, vy);
  float aFill = 1.0 - smoothstep(-1.0, 1.0, d);
  // COVERAGE GOES IN THE ALPHA AND NOWHERE ELSE. This blends with straight alpha
  // (GL_SRC_ALPHA), so `rgb` is a colour and `a` is how much of it lands — multiplying the colour
  // by the coverage too spends it twice, and the fragment arrives at `fill.rgb * aFill^2 * fill.a`.
  // On a DARK fill that is invisible, because black times anything is black, which is why it
  // survived every scrim, card and capsule this app has ever drawn. The moment a fill is LIGHT it
  // is a one-pixel BLACK RING around every rounded corner: measured on the light tab track over
  // bright artwork, the boundary pixel read 158 between a ground of 220 and a face of 229 — darker
  // than either side of it, which is not an edge any lamp in this design could cast. Reported as
  // "black outlines on the bar's roundings"; it was never the antialiasing.
  vec3 rgb = fill.rgb;
  float a = aFill * fill.a;
  // THE RIM, and the division is the whole of it. This fragment is blended with straight alpha
  // (GL_SRC_ALPHA), so what reaches the screen is `rgb * a` — and adding the rim to `rgb` therefore
  // draws it at `rim * a`, not at `rim`. With an OPAQUE fill a is 1 and the two are the same, which
  // is why every tile's edge-sheen has always looked right and why this went unnoticed. Over a
  // TRANSLUCENT fill it silently under-delivers (the flat tab track's .22 sheen was landing at
  // .10), and over a HOLLOW one — `Painter::rring`, and a container's rim drawn on its own — it
  // inverts: `rgb` is `rimcol * rim` and `a` is `rim`, so the line arrives at `rim` squared while
  // still darkening the destination by `1 - rim`. A translucent stroke was drawing a shadow.
  //
  // Dividing by the output alpha cancels the blend's own multiply, so `u_rimcol.a` is the weight
  // the line lands at, over any fill. Opaque callers are bit-identical: a == 1.
  float rimShape = smoothstep(-u_rimw - 0.75, -u_rimw + 0.75, d) * (1.0 - smoothstep(-0.5, 0.5, d));
  float rim = rimShape * u_rimcol.a;
  if (u_rimtop > 0.001) {
    // +y is DOWN here (v_uv runs top to bottom), so the top edge's normal is (0,-1) and this is 1
    // there, 0 at the widest point of either cap, and 0 across the whole bottom. Continuous by
    // construction: the extra weight dies out along the arc instead of being cut off on it.
    highp vec2 nrm = pill ? pillNormal(p) : sdBoxNormal(p, hsz, rad);
    rim += rimShape * u_rimtop * max(-nrm.y, 0.0);
  }
  // …and the glow, which is the same light one step further in: a falloff from the edge INWARD,
  // weighted by the surface normal so each side dies out where the face turns away — the reason the
  // rim's own top boost is built that way, and for the same reason (a hard cut lands mid-arc and
  // reads as the outline breaking off). It lives ON the face, so it is scaled by the fill's own
  // coverage rather than by the rim's shape.
  float glow = 0.0;
  if (u_glow.y > 0.001 || u_glow.w > 0.001) {
    highp vec2 gn = pill ? pillNormal(p) : sdBoxNormal(p, hsz, rad);
    highp float inw = max(-d, 0.0);
    glow = ((1.0 - smoothstep(0.0, max(u_glow.x, 0.001), inw)) * u_glow.y * max(-gn.y, 0.0)
          + (1.0 - smoothstep(0.0, max(u_glow.z, 0.001), inw)) * u_glow.w * max(gn.y, 0.0)) * aFill;
  }
  a = max(a, max(rim, glow));
  rgb += u_rimcol.rgb * ((rim + glow) / max(a, 1.0 / 512.0));
  if (u_focus > 0.001) {
    float ring = (1.0 - smoothstep(1.5, 4.0, abs(d - 5.0))) * u_focus;
    float glow = exp(-max(d, 0.0) / 14.0) * 0.40 * u_focus * step(0.0, d);
    rgb += (vec3(1.0) * ring + vec3(0.85, 0.9, 1.0) * glow) * u_focus_rgb;
    a = max(a, max(ring, glow));
  }
  gl_FragColor = vec4(plx_dither(rgb), a);
}
