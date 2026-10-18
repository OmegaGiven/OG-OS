// Neo-Tokyo frame pass: the cached city (city.frag) above the waterline, and
// below it the bay - the cached city mirrored, broken up by swell and rain
// rings. Rain and petals are drawn on top as sprites (rain.*, petal.*).
//
// u[0] w, h, t (s, wraps hourly), 1 = shader must sRGB-encode
// u[1] horizon y, -, -, -

uniform mediump sampler2D cache; // city at half brightness, top row first

vec3 cityc(ivec2 p) {
    return texelFetch(cache, p, 0).rgb * 2.0;
}

// Expanding rain-ring field on the water: (ring brightness, ripple slope).
// Each ring stays inside its own cell, so only one cell is checked.
vec2 rain_rings(vec2 p, float depth01, float t) {
    // Perspective: cells grow toward the viewer, rings are flattened ellipses.
    float cs = mix(18.0, 90.0, depth01);
    vec2 q = vec2(p.x, p.y * 3.2) / cs;
    vec2 cell = floor(q);
    vec2 h = h22(cell, 12u);
    float period = 0.9 + 1.4 * h.y;
    float age = fract(t / period + h.x);
    vec2 center = cell + 0.5 + 0.1 * (h22(cell, 13u) - 0.5);
    float r = length(q - center);
    float rr = age * 0.42;
    float fade = (1.0 - age) * (1.0 - age) * step(h.x, 0.7);
    float band = exp(-abs(r - rr) * 28.0) * fade;
    return vec2(band, sin((r - rr) * 40.0) * band);
}

void main() {
    vec2 frag = vec2(gl_FragCoord.x, u[0].y - gl_FragCoord.y);
    ivec2 px = ivec2(frag);
    float t = u[0].z;
    float horizon = u[1].x;
    vec3 col;
    if (frag.y < horizon + 3.0) {
        col = cityc(px);
    } else {
        float depth = frag.y - horizon;
        float depth01 = clamp(depth / (u[0].y - horizon), 0.0, 1.0);
        vec2 rr = rain_rings(frag, depth01, t);
        float swell = sin(frag.y * 0.35 + t * 1.7) * 0.6 + sin(frag.y * 0.11 - t * 0.9 + frag.x * 0.01) * 1.2;
        float dx = (swell + rr.y * 6.0) * (0.4 + depth01 * 1.6);
        int sx = clamp(int(frag.x + dx), 0, int(u[0].x) - 1);
        float sy = horizon - depth * 0.92 - 4.0;
        // Lights smear into vertical streaks on the water: average two taps
        // reaching further up the mirrored city.
        float stretch = 4.0 + depth01 * 14.0;
        int y0 = clamp(int(sy), 0, int(horizon) - 6);
        int y1 = clamp(int(sy - stretch), 0, int(horizon) - 6);
        vec3 refl = (cityc(ivec2(sx, y0)) + cityc(ivec2(sx, y1))) * 0.5;
        float fresnel = mix(0.5, 0.22, depth01);
        col = vec3(0.006, 0.008, 0.02) + refl * fresnel * vec3(0.85, 0.9, 1.05);
        col += vec3(0.5, 0.45, 0.65) * rr.x * 0.06 * (0.4 + length(refl) * 2.0);
    }
    finish(col);
}
