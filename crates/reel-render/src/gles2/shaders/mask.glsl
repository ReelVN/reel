#version 100
precision highp float;
#ifndef REEL_MASK_NODES
#define REEL_MASK_NODES 32
#endif
uniform sampler2D u_image0,u_image1,u_image2,u_image3;
uniform vec4 u_ops[REEL_MASK_NODES],u_texels[4],u_dimensions[4];
uniform vec4 u_uv0,u_uv1,u_values,u_flags;
uniform vec2 u_output_size;
uniform int u_count;
vec4 image_sample(int image,vec2 uv){if(image==0)return texture2D(u_image0,uv);if(image==1)return texture2D(u_image1,uv);if(image==2)return texture2D(u_image2,uv);return texture2D(u_image3,uv);}
vec3 encoded(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}
float smooth_mask(float a,float b,float x){if(a==b)return step(a,x);float t=clamp((x-a)/(b-a),0.0,1.0);return t*t*(3.0-2.0*t);}

// A local array with a dynamic subscript becomes indexed temporary GPU memory
// on SGX. Explicit vector banks keep every value in registers. The loop still
// reads immutable uniform instructions; expanding its evaluator caused spills.
vec4 bank0,bank1;
#if REEL_MASK_NODES > 8
vec4 bank2,bank3;
#endif
#if REEL_MASK_NODES > 16
vec4 bank4,bank5,bank6,bank7;
#endif

// Select components without arithmetic mixing: an unused NaN or infinity must
// not contaminate a different node. All references name earlier validated slots.
float read_lane(vec4 value,float lane){
 if(lane<0.5)return value.x;
 if(lane<1.5)return value.y;
 if(lane<2.5)return value.z;
 return value.w;
}
float read_node(float index){
 if(index<4.0)return read_lane(bank0,index);
#if REEL_MASK_NODES > 8
 if(index<8.0)return read_lane(bank1,index-4.0);
 if(index<12.0)return read_lane(bank2,index-8.0);
#if REEL_MASK_NODES > 16
 if(index<16.0)return read_lane(bank3,index-12.0);
 if(index<20.0)return read_lane(bank4,index-16.0);
 if(index<24.0)return read_lane(bank5,index-20.0);
 if(index<28.0)return read_lane(bank6,index-24.0);
 return read_lane(bank7,index-28.0);
#else
 return read_lane(bank3,index-12.0);
#endif
#else
 return read_lane(bank1,index-4.0);
#endif
}
vec4 write_lane(vec4 bank,float lane,float value){
 if(lane<0.5)bank.x=value;
 else if(lane<1.5)bank.y=value;
 else if(lane<2.5)bank.z=value;
 else bank.w=value;
 return bank;
}
void write_node(int index,float value){
 float f=float(index);
 if(index<4){bank0=write_lane(bank0,f,value);return;}
#if REEL_MASK_NODES > 8
 if(index<8){bank1=write_lane(bank1,f-4.0,value);return;}
 if(index<12){bank2=write_lane(bank2,f-8.0,value);return;}
#if REEL_MASK_NODES > 16
 if(index<16){bank3=write_lane(bank3,f-12.0,value);return;}
 if(index<20){bank4=write_lane(bank4,f-16.0,value);return;}
 if(index<24){bank5=write_lane(bank5,f-20.0,value);return;}
 if(index<28){bank6=write_lane(bank6,f-24.0,value);return;}
 bank7=write_lane(bank7,f-28.0,value);
#else
 bank3=write_lane(bank3,f-12.0,value);
#endif
#else
 bank1=write_lane(bank1,f-4.0,value);
#endif
}
float evaluate(vec4 a,vec2 uv){
 int op=int(a.x);float value=0.0;
  if(op==0)value=a.y;
  else if(op==1){if(a.y<0.5)value=u_values.x;else if(a.y<1.5)value=u_values.y;else if(a.y<2.5)value=u_values.z;else value=u_values.w;}
  else if(op==2||op==3){
   int image=int(a.y);vec2 offset=vec2(read_node(mod(a.z,32.0)),read_node(floor(a.z/32.0)));
   vec2 scale=vec2(read_node(mod(a.w,32.0)),read_node(mod(floor(a.w/32.0),32.0)));
   float repeat=floor(a.w/1024.0);vec2 at=uv*scale+offset;
   if(mod(repeat,2.0)>0.5)at.x=fract(at.x);if(repeat>1.5)at.y=fract(at.y);
   vec4 texels=u_texels[image],dimensions=u_dimensions[image];vec2 half_texel=0.5/dimensions.xy;
   at=clamp(at*texels.xy,half_texel,texels.xy-half_texel);
   vec4 color=image_sample(image,at);
   if(texels.w==1.0)color.a=1.0;if(texels.w==2.0)color.rgb=min(color.rgb,vec3(color.a));
   if(op==2)value=color.a;
   else {vec3 rgb=color.a>0.0?clamp(color.rgb/color.a,0.0,1.0):vec3(0.0);if(texels.z==0.0)rgb=encoded(rgb);value=dot(rgb,vec3(0.2126,0.7152,0.0722));}
  }
  else if(op==4)value=read_node(a.y)+read_node(a.z);
  else if(op==5)value=read_node(a.y)-read_node(a.z);
  else if(op==6)value=read_node(a.y)*read_node(a.z);
  else if(op==7)value=a.z==0.0?1.0:pow(max(read_node(a.y),0.0),a.z);
  else if(op==8)value=clamp(read_node(a.y),a.z,a.w);
  else if(op==9)value=smooth_mask(read_node(a.y),read_node(a.z),read_node(a.w));
  else {float gray=read_node(a.z);if(u_flags.x!=0.0)gray=1.0-gray;float threshold=step(gray,u_values.x);if(u_values.y>0.0)threshold=clamp((u_values.x*(1.0+u_values.y)-gray)/u_values.y,0.0,1.0);value=read_node(a.y)*threshold;if(u_values.x<=0.0)value=0.0;if(u_values.x>=1.0)value=1.0;}
 return value;
}
void main(){
 vec3 position=vec3(gl_FragCoord.x,u_output_size.y-gl_FragCoord.y,1.0);
 vec2 uv=vec2(dot(u_uv0.xyz,position),dot(u_uv1.xyz,position));
 if(any(lessThan(uv,vec2(0.0)))||any(greaterThan(uv,vec2(1.0)))){gl_FragColor=vec4(0.0);return;}
 bank0=vec4(0.0);bank1=vec4(0.0);
#if REEL_MASK_NODES > 8
 bank2=vec4(0.0);bank3=vec4(0.0);
#endif
#if REEL_MASK_NODES > 16
 bank4=vec4(0.0);bank5=vec4(0.0);bank6=vec4(0.0);bank7=vec4(0.0);
#endif
 float value=0.0;
 for(int i=0;i<REEL_MASK_NODES;i++){
  if(i>=u_count)break;
  value=evaluate(u_ops[i],uv);
  write_node(i,value);
 }
 gl_FragColor=vec4(clamp(value,0.0,1.0));
}
