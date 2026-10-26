#version 100
precision highp float;
#ifndef REEL_MASK_FETCHES
#define REEL_MASK_FETCHES 2
#endif
#ifndef REEL_MASK_MATH
#define REEL_MASK_MATH 16
#endif
uniform sampler2D u_image0,u_image1;
uniform vec4 u_ops[REEL_MASK_MATH];
uniform vec4 u_coordinates[REEL_MASK_FETCHES],u_fetches[REEL_MASK_FETCHES];
uniform vec4 u_texels[REEL_MASK_FETCHES],u_dimensions[REEL_MASK_FETCHES];
uniform vec4 u_seed0,u_seed1,u_uv0,u_uv1,u_flags;
uniform vec2 u_output_size;
vec4 bank0,bank1;

float read_lane(vec4 value,float lane){
 if(lane<0.5)return value.x;
 if(lane<1.5)return value.y;
 if(lane<2.5)return value.z;
 return value.w;
}
float read_node(float index){
 if(index<4.0)return read_lane(bank0,index);
 return read_lane(bank1,index-4.0);
}
vec4 write_lane(vec4 bank,float lane,float value){
 if(lane<0.5)bank.x=value;
 else if(lane<1.5)bank.y=value;
 else if(lane<2.5)bank.z=value;
 else bank.w=value;
 return bank;
}
void write_node(float index,float value){
 if(index<4.0)bank0=write_lane(bank0,index,value);
 else bank1=write_lane(bank1,index-4.0,value);
}
vec3 encoded(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}
float smooth_mask(float a,float b,float x){if(a==b)return step(a,x);float t=clamp((x-a)/(b-a),0.0,1.0);return t*t*(3.0-2.0*t);}
vec2 coordinates(vec2 uv,vec4 c,float repeat,vec4 texels,vec2 dimensions){
 vec2 at=uv*c.zw+c.xy;
 if(mod(repeat,2.0)>0.5)at.x=fract(at.x);
 if(repeat>1.5)at.y=fract(at.y);
 vec2 half_texel=0.5/dimensions;
 return clamp(at*texels.xy,half_texel,texels.xy-half_texel);
}
void sampled(vec4 color,vec4 texels,vec4 fetch){
 if(texels.w==1.0)color.a=1.0;
 if(texels.w==2.0)color.rgb=min(color.rgb,vec3(color.a));
 if(fetch.y>=0.0)write_node(fetch.y,color.a);
#ifndef REEL_MASK_ALPHA_ONLY
 if(fetch.z>=0.0){
  vec3 rgb=color.a>0.0?clamp(color.rgb/color.a,0.0,1.0):vec3(0.0);
  if(texels.z==0.0)rgb=encoded(rgb);
  write_node(fetch.z,dot(rgb,vec3(0.2126,0.7152,0.0722)));
 }
#endif
}
float evaluate(vec4 a){
 int op=int(a.x);
 if(op==4)return read_node(a.y)+read_node(a.z);
 if(op==5)return read_node(a.y)-read_node(a.z);
 if(op==6)return read_node(a.y)*read_node(a.z);
 if(op==7)return a.z==0.0?1.0:pow(max(read_node(a.y),0.0),a.z);
 if(op==8)return clamp(read_node(a.y),a.z,a.w);
 return smooth_mask(read_node(a.y),read_node(a.z),read_node(a.w));
}
void main(){
 vec3 position=vec3(gl_FragCoord.x,u_output_size.y-gl_FragCoord.y,1.0);
 vec2 uv=vec2(dot(u_uv0.xyz,position),dot(u_uv1.xyz,position));
 if(any(lessThan(uv,vec2(0.0)))||any(greaterThan(uv,vec2(1.0)))){gl_FragColor=vec4(0.0);return;}
 bank0=u_seed0;bank1=u_seed1;
 // Fixed sampler and metadata slots remove sampling from opcode dispatch.
 sampled(texture2D(u_image0,coordinates(uv,u_coordinates[0],u_fetches[0].x,u_texels[0],u_dimensions[0].xy)),u_texels[0],u_fetches[0]);
#if REEL_MASK_FETCHES > 1
 sampled(texture2D(u_image1,coordinates(uv,u_coordinates[1],u_fetches[1].x,u_texels[1],u_dimensions[1].xy)),u_texels[1],u_fetches[1]);
#endif
 for(int i=0;i<REEL_MASK_MATH;i++){
  if(float(i)>=u_flags.x)break;
  vec4 instruction=u_ops[i];
  float destination=floor(instruction.x/16.0);
  instruction.x-=destination*16.0;
  float value=evaluate(instruction);
  write_node(destination,value);
 }
 gl_FragColor=vec4(clamp(read_node(u_flags.y),0.0,1.0));
}
