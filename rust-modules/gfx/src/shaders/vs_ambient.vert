// Four-corner field vertex shader (pairs with fs_ambient.frag): the same unit -> pixel rect mapping
// as vs_src.vert, drawn over `gfx::field_mesh` (FIELD_N² cells) rather than one quad, and the WHOLE
// bilinear field `mix(mix(tl, tr, u), mix(bl, br, u), v)` evaluated here, per vertex.
//
// History, all measured on the television with the HWCNT vinstr profiler: the field was first three mixes
// per fragment (3.2M GPU cycles a frame for the hero's corner scrim alone, 2026-09-02); then the two
// horizontal mixes moved here as exact varyings over one quad, leaving one mix and three varyings
// per fragment. On the mesh a triangle's linear interpolation is within half an 8-bit code of the
// field (`gfx.rs`'s mesh test), so the fragment reads ONE varying and does no colour arithmetic:
// ~0.4M fewer cycles a frame for Home's full-screen wash (2026-09-19).
//
// THE GROUND SHADE (`PLX_SCRIM`, `gfx/scrim.rs`): up to six soft rounded blocks of one ink, one per
// piece of content a hero draws on the art, each evaluated HERE, per vertex, and stacked
// `1 - prod(1 - a_i)` into the ink alpha the wash already carries. It is the ramp's own move for a
// field no straight ramp describes, so the fragment still reads one varying and does no colour
// arithmetic. A block is a rounded rectangle's signed distance `sd` (centre and the half extents of
// the rectangle its corner arcs are centred on in the first vec4, the radius, 1/feather and peak in
// the second) and the alpha `peak * (1 - smoothstep(0, feather, sd))`; an unused block has peak 0
// and adds exactly nothing. With every uniform zero the stack is exactly 0.0 and
// `mix(inka, 1.0, 0.0)` is the ramp's alpha to the bit, so a screen that has no shade draws what it
// always drew. The live blocks are the FIRST `u_scn` of the six (`GroundShade::live`), and the stack
// stops there: a ground with no shade pays one compare a vertex instead of six distances, a hero
// with three blocks pays three. A block past the count is a factor of exactly 1.0, so stopping
// changes no bit of the product.
attribute vec2 a_pos;
uniform vec4 u_rect;
uniform vec2 u_screen;
uniform vec4 u_atl, u_atr, u_abr, u_abl;
varying vec4 v_col;
#ifdef PLX_DITHER_NC
// The dither tile's coordinate (`shaders/dither.glsl`, cost rule 4): target px / NOISE_DIM, linear
// in position, so interpolated exactly and never computed per fragment. Only the programs whose
// vertex source is built with `gfx::glsl_vs_dithered!` carry it.
varying highp vec2 v_dither_nc;
#endif
#ifdef PLX_WASH_INK
// THE INK RAMP ON THE WASH (`gfx::draw_ambient_inked`): a vertical scrim ramp of one ink laid over
// the field — the screen's atmospheric ramp, which used to be one or two more full-width blended
// passes over the same pixels. Its alpha runs LINEARLY from `u_inka.x` at this rect's top to
// `u_inka.y` at its bottom (the caller cuts the ramp at its knees, so each rect is one straight
// segment), which makes it exact per vertex, like the field itself.
#ifndef PLX_ART_WASH
uniform vec3 u_ink;
#endif
uniform vec2 u_inka;
#endif
#ifdef PLX_SCRIM
// Six blocks, two vec4 each: (cx, cy, hx - r, hy - r) and (r, 1/feather, peak, 0), in device pixels.
uniform vec4 u_sc[12];
// How many of them are live, from the first: 0 for no shade at all.
uniform float u_scn;
float shade_block(int i, vec2 px) {
  vec2 d = max(abs(px - u_sc[i].xy) - u_sc[i].zw, 0.0);
  float t = clamp((length(d) - u_sc[i + 1].x) * u_sc[i + 1].y, 0.0, 1.0);
  return u_sc[i + 1].z * (1.0 - t * t * (3.0 - 2.0 * t));
}
// An edge field packs horizontal and vertical easing, three shelf stops, a soft join and a top fade.
float edge_alpha(vec2 px) {
  vec4 edge = u_sc[0];
  vec4 shelf = u_sc[1];
  vec4 ink = u_sc[2];
  float upper = ink.z * (1.0 - smoothstep(edge.x, edge.y, px.x)) * smoothstep(edge.z, edge.w, px.y);
  vec3 top = u_sc[3].xyz;
  if (top.z > 0.0) upper += top.z * (1.0 - smoothstep(top.x, top.y, px.y));
  float foot = px.y < shelf.y
    ? mix(shelf.w, ink.x, clamp((px.y - shelf.x) / (shelf.y - shelf.x), 0.0, 1.0))
    : mix(ink.x, ink.y, clamp((px.y - shelf.y) / (shelf.z - shelf.y), 0.0, 1.0));
  float blend = max(0.0, ink.w - abs(upper - foot));
  return min(upper + foot, max(upper, foot) + blend * blend * 0.25 / ink.w);
}
float scrim_alpha(vec2 px) {
  if (u_scn < 0.0) return edge_alpha(px);
  float keep = 1.0;
  for (int i = 0; i < 6; i++) {
    if (float(i) >= u_scn) break;
    keep *= 1.0 - shade_block(2 * i, px);
  }
  return 1.0 - keep;
}
#endif
#ifdef PLX_SCRIM_LAYER
// The scrim drawn ALONE over what is under it (the layered frames: a hero slide's two art layers):
// the ink and its stacked alpha, as the colour the plain program blends.
uniform vec3 u_ink;
#endif
#ifdef PLX_ART_WASH
// The ramp goes OVER the art, so the art-wash fragment applies it: hand it the alpha.
varying float v_inka;
// The wash with a photograph dissolved INTO it (`fs_art_wash.frag`, `gfx::draw_art_wash`): the
// same field over the same mesh, also handing the fragment the photograph's texture coordinate.
// It is linear in position, so — like the noise coordinate — it interpolates exactly and the
// fragment does no arithmetic to find it.
uniform highp vec4 u_art;    // the art quad: (x, y, 1/w, 1/h) in authored pixels
uniform highp vec4 u_uvrect; // the texture window the quad samples: (u, v, du, dv)
varying highp vec2 v_cuv;
#endif
void main(){
  vec2 px = u_rect.xy + a_pos * u_rect.zw;
#ifdef PLX_SCRIM_LAYER
  v_col = vec4(u_ink, scrim_alpha(px));
#else
  v_col = mix(mix(u_atl, u_atr, a_pos.x), mix(u_abl, u_abr, a_pos.x), a_pos.y);
#endif
#ifdef PLX_DITHER_NC
  v_dither_nc = px * (1.0 / 256.0);
#endif
#ifdef PLX_WASH_INK
  float inka = mix(u_inka.x, u_inka.y, a_pos.y);
#ifdef PLX_SCRIM
  inka = mix(inka, 1.0, scrim_alpha(px));
#endif
#ifdef PLX_ART_WASH
  v_inka = inka;
#else
  v_col.rgb = mix(v_col.rgb, u_ink, inka);
#endif
#endif
#ifdef PLX_ART_WASH
  v_cuv = u_uvrect.xy + (px - u_art.xy) * u_art.zw * u_uvrect.zw;
#endif
  vec2 ndc = px / u_screen * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
