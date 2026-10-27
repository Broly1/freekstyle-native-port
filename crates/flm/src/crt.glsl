// CRT TV look in the style of RetroArch's crt-lottes, flat screen: gaussian
// beam (scanlines vertically), aperture-grille phosphor mask and a little
// glow. mpv/libplacebo hook, renders the 4:3 output straight from the low-res
// source. flm2mkv --crt replaces the WIDTH/HEIGHT below with its output size;
// to preview: mpv --glsl-shaders=crt.glsl movie.mkv (a 512x448 conversion).

//!HOOK MAIN
//!BIND HOOKED
//!DESC CRT
//!WIDTH 1440
//!HEIGHT 1080

#define LINES      224.0   // scanlines across the screen (one per field line)
#define HARD_SCAN  -8.0    // scanline sharpness (more negative = thinner beams)
#define HARD_PIX   -3.5    // horizontal beam sharpness
#define MASK_DARK  0.6
#define MASK_LIGHT 1.4
#define GLOW       0.12
#define BRIGHT     1.45    // makes up for the light lost to scanlines and mask

vec2 src;

// Clamped: video YUV can decode to slightly negative RGB, and pow() of that
// is NaN, which the beam filter then smears into coloured blocks.
vec3 lin(vec3 c) { return pow(clamp(c, 0.0, 1.0), vec3(2.2)); }

// One source pixel of the virtual LINES-tall grid, in linear light.
vec3 fetch(vec2 cell) {
    vec2 uv = (cell + 0.5) / src;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) return vec3(0.0);
    return lin(HOOKED_tex(uv).rgb);
}

// Horizontal beam through one scanline, 5 taps.
vec3 horz(vec2 pos, float dy, float hard) {
    vec2 p = pos * src - 0.5;
    vec2 base = floor(p);
    float fx = p.x - base.x;
    vec3 sum = vec3(0.0);
    float wsum = 0.0;
    for (int i = -2; i <= 2; i++) {
        float d = float(i) - fx;
        float w = exp2(hard * d * d);
        sum += fetch(base + vec2(float(i), dy)) * w;
        wsum += w;
    }
    return sum / wsum;
}

float scan(vec2 pos, float dy, float hard) {
    float d = fract(pos.y * src.y - 0.5) - dy;
    return exp2(hard * d * d);
}

vec3 tri(vec2 pos) {
    return horz(pos, -1.0, HARD_PIX) * scan(pos, -1.0, HARD_SCAN)
         + horz(pos,  0.0, HARD_PIX) * scan(pos,  0.0, HARD_SCAN)
         + horz(pos,  1.0, HARD_PIX) * scan(pos,  1.0, HARD_SCAN);
}

// Wide soft blur for the glow around bright parts.
vec3 glow(vec2 pos) {
    return horz(pos, -1.0, -0.6) * scan(pos, -1.0, -1.5)
         + horz(pos,  0.0, -0.6) * scan(pos,  0.0, -1.5)
         + horz(pos,  1.0, -0.6) * scan(pos,  1.0, -1.5);
}

// Aperture grille: vertical R, G, B stripes one output pixel wide.
vec3 mask(vec2 frag) {
    float x = fract(frag.x / 3.0);
    vec3 m = vec3(MASK_DARK);
    if (x < 0.333) m.r = MASK_LIGHT;
    else if (x < 0.666) m.g = MASK_LIGHT;
    else m.b = MASK_LIGHT;
    return m;
}

vec4 hook() {
    src = vec2(HOOKED_size.x, LINES);
    vec2 pos = HOOKED_pos;
    vec3 c = tri(pos) * mask(gl_FragCoord.xy) * BRIGHT + glow(pos) * GLOW;

    return vec4(pow(clamp(c, 0.0, 1.0), vec3(1.0 / 2.2)), 1.0);
}
