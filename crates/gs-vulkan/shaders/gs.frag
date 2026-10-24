#version 450

// GS pixel pipeline: texture function, fog, alpha test, blend source scale.
// Work in GS units: colours 0..255, alpha 0x80 = 1.0.

layout(location = 0) noperspective in vec4 v_color;
layout(location = 1) noperspective in vec3 v_stq;
layout(location = 2) noperspective in float v_fog;

layout(set = 0, binding = 0) uniform sampler2D tex;

layout(push_constant) uniform Push {
    vec2 rt_size;
    vec2 tex_scale;
    vec4 fogcol;
    uint flags;
    float aref;
    float fix;
} pc;

layout(location = 0) out vec4 o_color;

// flags layout (see hw.rs):
//  0     TME      1-2  TFX     3  TCC     4  FGE
//  5     ATE      6-8  ATST    9-10 AFAIL
//  11-12 source scale: 0 none, 1 (1+As), 2 (1+FIX)
const uint TME = 1u, TCC = 8u, FGE = 16u, ATE = 32u;

bool alpha_pass(float a) {
    float r = pc.aref;
    switch ((pc.flags >> 6) & 7u) {
        case 0u: return false;
        case 1u: return true;
        case 2u: return a < r;
        case 3u: return a <= r;
        case 4u: return a == r;
        case 5u: return a >= r;
        case 6u: return a > r;
        default: return a != r;
    }
}

void main() {
    vec4 c = floor(v_color * 255.0 + 0.5);
    if ((pc.flags & TME) != 0u) {
        vec4 t = floor(texture(tex, v_stq.xy / v_stq.z * pc.tex_scale) * 255.0 + 0.5);
        bool tcc = (pc.flags & TCC) != 0u;
        switch ((pc.flags >> 1) & 3u) {
            case 0u: // MODULATE
                c.rgb = c.rgb * t.rgb / 128.0;
                c.a = tcc ? t.a * c.a / 128.0 : c.a;
                break;
            case 1u: // DECAL
                c.rgb = t.rgb;
                c.a = tcc ? t.a : c.a;
                break;
            case 2u: // HIGHLIGHT
                c.rgb = c.rgb * t.rgb / 128.0 + c.a;
                c.a = tcc ? t.a + c.a : c.a;
                break;
            default: // HIGHLIGHT2
                c.rgb = c.rgb * t.rgb / 128.0 + c.a;
                c.a = tcc ? t.a : c.a;
                break;
        }
        c = min(floor(c), vec4(255.0));
    }
    if ((pc.flags & FGE) != 0u) {
        float f = v_fog;
        c.rgb = floor(c.rgb * f + pc.fogcol.rgb * 255.0 * (1.0 - f));
    }
    if ((pc.flags & ATE) != 0u && !alpha_pass(c.a)) {
        // AFAIL other than KEEP would still write some channels; treated as
        // a pass for now.
        if (((pc.flags >> 9) & 3u) == 0u) {
            discard;
        }
    }
    uint scale = (pc.flags >> 11) & 3u;
    if (scale == 1u) {
        c.rgb *= 1.0 + c.a / 128.0;
    } else if (scale == 2u) {
        c.rgb *= 1.0 + pc.fix;
    }
    o_color = vec4(c.rgb / 255.0, clamp(c.a / 128.0, 0.0, 1.0));
}
