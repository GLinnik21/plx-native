// Text vertex shader: unit quad -> the string texture's pixel rect, v_tuv passes the quad UV.
// v_ly carries the fragment's ABSOLUTE LOGICAL SCREEN Y (the same space `u_trect.y` is passed in,
// i.e. Painter's own coordinates) for the fade program's vertical edge bands — a coordinate chain,
// so highp per `ui/CLAUDE.md`'s Mali rule (see fs_text.frag's v_tuv note for the same reasoning).
// Shared by the plain program too; fs_text.frag simply never reads it, which GLSL ES permits.
attribute vec2 a_pos;
uniform vec4 u_trect;
uniform vec2 u_tscreen;
varying vec2 v_tuv;
varying highp float v_ly;
void main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;
  v_ly=px.y;
  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }
