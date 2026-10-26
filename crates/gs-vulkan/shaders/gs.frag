#version 450

// GS pixel pipeline: texture function, fog, alpha test, blend source scale.
// Work in GS units: colours 0..255, alpha 0x80 = 1.0. Targets and
// textures store the alpha byte as is; the blend factor (alpha / 0x80) goes
// out as the second blend source.

layout(location = 0) noperspective in vec4 v_color;
layout(location = 1) noperspective in vec3 v_stq;
layout(location = 2) noperspective in float v_fog;

layout(set = 0, binding = 0) uniform sampler2D tex;
// Snapshot of the target being drawn (destination alpha, masked alpha).
layout(set = 0, binding = 1) uniform sampler2D dst;
// Palette for T8h reads of a render target.
layout(set = 0, binding = 2) uniform sampler2D pal;

layout(push_constant) uniform Push {
    vec2 rt_size;
    vec2 tex_scale;
    vec4 fogcol;
    uint flags;
    float aref;
    float fix;
    uint amask;
} pc;

layout(location = 0, index = 0) out vec4 o_color;
layout(location = 0, index = 1) out vec4 o_blend;

// flags layout (see hw.rs):
//  0     TME      1-2  TFX     3  TCC     4  FGE
//  5     ATE      6-8  ATST    9-10 AFAIL
//  11-12 source scale: 0 none, 1 (1+As), 2 (1+FIX)
//  13    RTTEX    14   DATE     15   DATM     16   FBA     17   AMERGE
//  18    PAL8H    19-24 dest blend A B D     25   DBLEND
const uint TME = 1u, TCC = 8u, FGE = 16u, ATE = 32u, DATE = 16384u, DATM = 32768u;
const uint FBA = 65536u, AMERGE = 131072u, PAL8H = 262144u, DBLEND = 33554432u;

vec3 blend_sel(uint s, vec3 cs, vec3 cd) {
    return s == 0u ? cs : s == 1u ? cd : vec3(0.0);
}

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
    // Destination alpha test: targets store alpha / 0x80, so the GS alpha
    // bit (alpha >= 0x80) is a stored 1.0.
    if ((pc.flags & DATE) != 0u) {
        bool bit = texelFetch(dst, ivec2(gl_FragCoord.xy), 0).a >= 0.5;
        if (bit != ((pc.flags & DATM) != 0u)) {
            discard;
        }
    }
    vec4 c = floor(v_color * 255.0 + 0.5);
    if ((pc.flags & TME) != 0u) {
        vec2 uv = v_stq.xy / v_stq.z * pc.tex_scale;
        vec4 t;
        if ((pc.flags & PAL8H) != 0u) {
            ivec2 size = textureSize(tex, 0);
            ivec2 p = clamp(ivec2(floor(uv * vec2(size))), ivec2(0), size - 1);
            uint i = uint(texelFetch(tex, p, 0).a * 255.0 + 0.5);
            t = floor(texelFetch(pal, ivec2(int(i), 0), 0) * 255.0 + 0.5);
        } else {
            t = floor(texture(tex, uv) * 255.0 + 0.5);
        }
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
    // RGB_ONLY alpha test failure: the target keeps its alpha.
    bool keep_a = false;
    if ((pc.flags & ATE) != 0u && !alpha_pass(c.a)) {
        keep_a = ((pc.flags >> 9) & 3u) == 3u;
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
    if ((pc.flags & DBLEND) != 0u) {
        vec4 d = floor(texelFetch(dst, ivec2(gl_FragCoord.xy), 0) * 255.0 + 0.5);
        vec3 x = blend_sel((pc.flags >> 19) & 3u, c.rgb, d.rgb);
        vec3 y = blend_sel((pc.flags >> 21) & 3u, c.rgb, d.rgb);
        vec3 z = blend_sel((pc.flags >> 23) & 3u, c.rgb, d.rgb);
        c.rgb = clamp(floor((x - y) * d.a / 128.0) + z, 0.0, 255.0);
    }
    uint a = uint(clamp(c.a, 0.0, 255.0));
    if ((pc.flags & FBA) != 0u) {
        a |= 0x80u;
    }
    if ((pc.flags & AMERGE) != 0u) {
        uint d = uint(texelFetch(dst, ivec2(gl_FragCoord.xy), 0).a * 255.0 + 0.5);
        uint m = keep_a ? 0xffu : pc.amask;
        a = (a & ~m) | (d & m);
    }
    o_color = vec4(c.rgb / 255.0, float(a) / 255.0);
    o_blend = vec4(0.0, 0.0, 0.0, clamp(c.a / 128.0, 0.0, 1.0));
}
