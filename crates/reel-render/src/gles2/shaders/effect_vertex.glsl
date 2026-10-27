#version 100
precision highp float;
attribute vec2 a_position,a_uv;
uniform vec4 reel_context[9];
varying vec2 v_uv,reel_implicit_source_uv;
void main(){
 gl_Position=vec4(a_position,0.0,1.0);
 v_uv=a_uv;
 vec3 p=vec3(a_uv,1.0);
 vec2 source=vec2(dot(reel_context[7].xyz,p),dot(reel_context[8].xyz,p));
 reel_implicit_source_uv=vec2(source.x,1.0-source.y);
}
