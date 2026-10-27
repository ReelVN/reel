#version 100
precision highp float;
varying vec2 v_uv,v_source_uv;
uniform sampler2D u_source,u_mask,u_backdrop,u_expression;
uniform vec4 u_params;
uniform float u_invert,u_blend,u_has_expression;
vec4 sample_fbo(sampler2D image){return texture2D(image,v_uv);}
vec4 sample_source(){return texture2D(u_source,v_source_uv);}
vec3 encode(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}
vec3 linear(vec3 x){return mix(pow((x+0.055)/1.055,vec3(2.4)),x/12.92,step(x,vec3(0.04045)));}
#ifdef REEL_SOURCE_OVER_ONLY
void main(){gl_FragColor=sample_source()*u_params.x;}
#elif defined(REEL_ALPHA_MASK_ONLY)
void main(){gl_FragColor=sample_source()*clamp(sample_fbo(u_mask).a,0.0,1.0)*u_params.x;}
#else
#ifdef REEL_EXTENDED_BLEND
// W3C separable blend functions on straight linear RGB; linear burn extends this set.
vec3 overlay(vec3 b, vec3 s){return mix(2.0 * b * s, 1.0 - 2.0 * (1.0 - b) * (1.0 - s), step(vec3(0.5), b));}
vec3 hard_light(vec3 b, vec3 s){return mix(2.0 * b * s, 1.0 - 2.0 * (1.0 - b) * (1.0 - s), step(vec3(0.5), s));}
// Keep the divisor finite at source one; backdrop zero takes priority.
vec3 color_dodge(vec3 b, vec3 s){return max(min(vec3(1.0), b / max(1.0 - s, step(vec3(1.0), s))), step(vec3(1.0), s)) * sign(b);}
vec3 lighten(vec3 b, vec3 s){return max(b, s);}
vec3 difference(vec3 b, vec3 s){return abs(b - s);}
vec3 soft_curve(vec3 b){return mix(((16.0 * b - 12.0) * b + 4.0) * b, sqrt(b), step(vec3(0.25), b));}
vec3 soft_light(vec3 b, vec3 s){return mix(b - (1.0 - 2.0 * s) * b * (1.0 - b), b + (2.0 * s - 1.0) * (soft_curve(b) - b), step(vec3(0.5), s));}
vec3 linear_burn(vec3 b, vec3 s){return max(b + s - 1.0, vec3(0.0));}
vec4 separable(vec4 s,vec4 d){
 if(s.a==0.0)return d;
 if(d.a==0.0)return s;
 vec3 b=clamp(d.rgb/d.a,0.0,1.0),c=clamp(s.rgb/s.a,0.0,1.0),blended;
 if(u_blend==5.0)blended=overlay(b,c);
 else if(u_blend==6.0)blended=hard_light(b,c);
 else if(u_blend==7.0)blended=color_dodge(b,c);
 else if(u_blend==8.0)blended=lighten(b,c);
 else if(u_blend==9.0)blended=difference(b,c);
 else if(u_blend==10.0)blended=soft_light(b,c);
 else blended=linear_burn(b,c);
 return vec4(s.rgb * (1.0 - d.a) + d.rgb * (1.0 - s.a) + s.a * d.a * blended, s.a + d.a * (1.0 - s.a));
}
#endif
void main(){
 vec4 s=sample_source(),m=sample_fbo(u_mask);
 if(u_invert!=0.0&&s.a>0.0){vec3 encoded=encode(clamp(s.rgb/s.a,0.0,1.0));s.rgb=linear(mix(encoded,1.0-encoded,u_invert))*s.a;}
 float expression=u_has_expression>0.5?sample_fbo(u_expression).r:1.0;
 if(u_params.y==3.0)s=mix(s,m,u_params.z);
 else if(u_params.y==4.0)s=u_params.z<=0.0?s:(u_params.z>=1.0?m:mix(s,m,clamp(expression,0.0,1.0)));
 else {float coverage=1.0;if(u_params.y==1.0)coverage=clamp(m.a,0.0,1.0);if(u_params.y==2.0)coverage=clamp(dot(m.rgb,vec3(0.2126,0.7152,0.0722)),0.0,1.0);s*=coverage*u_params.x*expression;}
#ifdef REEL_EXTENDED_BLEND
 gl_FragColor=separable(s,sample_fbo(u_backdrop));
#else
 if(u_blend==0.0){gl_FragColor=s;return;}
 vec4 d=sample_fbo(u_backdrop);float alpha=s.a+d.a*(1.0-s.a);
 if(u_blend==1.0)gl_FragColor=min(s+d,1.0);
 else if(u_blend==4.0)gl_FragColor=vec4(max(d.rgb-s.rgb,vec3(0.0)),d.a);
 else if(u_blend==2.0)gl_FragColor=vec4(s.rgb*d.rgb+s.rgb*(1.0-d.a)+d.rgb*(1.0-s.a),alpha);
 else gl_FragColor=vec4(s.rgb+d.rgb-s.rgb*d.rgb,alpha);
#endif
}
#endif
