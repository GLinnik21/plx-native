// Four-corner gradient vertex shader (pairs with fs_ambient.frag): the same unit quad -> pixel rect
// mapping as vs_src.vert, plus the two HORIZONTAL corner mixes done here, per vertex, as varyings.
//
// A bilinear field is `mix(mix(tl, tr, u), mix(bl, br, u), v)`. The two inner mixes are linear in
// `u`, and a varying across a screen-aligned quad interpolates a linear function EXACTLY, so
// handing `top(u)` and `bot(u)` to the rasterizer reproduces them bit-for-bit at every fragment
// and leaves the fragment shader one mix instead of three. Measured on the television
// (`plxnative-hwcnt`, 2026-09-02): the three-mix fragment priced the hero's corner scrim at 3.2M
// GPU cycles a frame and the fold's full-screen wash higher still; the arithmetic pipe is what
// binds on this part, and the varying unit was idle. Same picture, a third of the words.
attribute vec2 a_pos;
uniform vec4 u_rect;
uniform vec2 u_screen;
uniform vec4 u_atl, u_atr, u_abr, u_abl;
varying vec2 v_uv;
varying vec4 v_top;
varying vec4 v_bot;
void main(){
  v_uv = a_pos;
  v_top = mix(u_atl, u_atr, a_pos.x);
  v_bot = mix(u_abl, u_abr, a_pos.x);
  vec2 px = u_rect.xy + a_pos * u_rect.zw;
  vec2 ndc = px / u_screen * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
