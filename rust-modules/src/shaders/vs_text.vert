// Text vertex shader: unit quad -> the string texture's pixel rect, v_tuv passes the quad UV.
attribute vec2 a_pos;
uniform vec4 u_trect;
uniform vec2 u_tscreen;
varying vec2 v_tuv;
void main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;
  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }
