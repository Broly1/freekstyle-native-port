#version 450

// Positions are GS window coordinates; colours keep GS units (alpha 0x80 = 1).
layout(location = 0) in vec2 in_pos;
layout(location = 1) in float in_z;
layout(location = 2) in vec4 in_color;
layout(location = 3) in vec3 in_stq;
layout(location = 4) in float in_fog;

layout(push_constant) uniform Push {
    vec2 rt_size;
    vec2 tex_scale;
    vec4 fogcol;
    uint flags;
    float aref;
    float fix;
} pc;

layout(location = 0) noperspective out vec4 v_color;
layout(location = 1) noperspective out vec3 v_stq;
layout(location = 2) noperspective out float v_fog;

void main() {
    gl_Position = vec4(in_pos / pc.rt_size * 2.0 - 1.0, in_z, 1.0);
    v_color = in_color;
    v_stq = in_stq;
    v_fog = in_fog;
}
