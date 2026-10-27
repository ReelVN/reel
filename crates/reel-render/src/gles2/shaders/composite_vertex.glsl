#version 100
precision highp float;
attribute vec2 a_position,a_uv;
uniform vec4 u_source_uv;
varying vec2 v_uv,v_source_uv;
void main(){
 gl_Position=vec4(a_position,0.0,1.0);
 v_uv=vec2(a_uv.x,1.0-a_uv.y);
 vec2 source=a_uv*u_source_uv.xy+u_source_uv.zw;
 v_source_uv=vec2(source.x,1.0-source.y);
}
