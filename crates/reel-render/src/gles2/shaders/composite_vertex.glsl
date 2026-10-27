#version 100
precision highp float;
attribute vec2 a_position,a_uv;
uniform vec4 u_source_uv,u_mask_uv,u_backdrop_uv,u_expression_uv;
varying vec2 v_source_uv,v_mask_uv,v_backdrop_uv,v_expression_uv;
void main(){
 gl_Position=vec4(a_position,0.0,1.0);
 vec2 frame_uv=vec2(a_uv.x,1.0-a_uv.y);
 v_mask_uv=frame_uv*u_mask_uv.xy+u_mask_uv.zw;
 v_backdrop_uv=frame_uv*u_backdrop_uv.xy+u_backdrop_uv.zw;
 v_expression_uv=frame_uv*u_expression_uv.xy+u_expression_uv.zw;
 vec2 source=a_uv*u_source_uv.xy+u_source_uv.zw;
 v_source_uv=vec2(source.x,1.0-source.y);
}
