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
void main(){
  v_col = mix(mix(u_atl, u_atr, a_pos.x), mix(u_abl, u_abr, a_pos.x), a_pos.y);
  vec2 px = u_rect.xy + a_pos * u_rect.zw;
#ifdef PLX_DITHER_NC
  v_dither_nc = px * (1.0 / 256.0);
#endif
  vec2 ndc = px / u_screen * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
