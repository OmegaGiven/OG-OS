// Neo-Tokyo city cache pass: the skyline (everything above the waterline)
// with its lights animated - flickering neon, animated billboards, blinking
// aviation lights. Rendered into an offscreen sRGB texture ~15x a second
// (lights change slowly); every frame then just reads it (base.frag).
// Stored at half brightness so lights brighter than 1.0 survive the 8-bit
// target.
//
// u[0] w, h, t (s, wraps hourly), -
// Rows here are top-left-origin texture rows (no flip).

uniform mediump sampler2D color_tex; // sRGB -> linear on load; a = emissive share
uniform highp usampler2D data_tex;   // kind | id<<3 | u<<16 | v<<24

const uint KIND_WINDOW = 1u;
const uint KIND_SIGN = 2u;
const uint KIND_BILLBOARD = 3u;
const uint KIND_BLINKER = 4u;
const uint KIND_TOWER = 5u;
const uint KIND_STAR = 6u;

vec3 hsv(float h, float s, float v) {
    vec3 k = vec3(1.0, 2.0 / 3.0, 1.0 / 3.0);
    vec3 p = abs(fract(vec3(h) + k) * 6.0 - 3.0);
    return v * mix(vec3(1.0), clamp(p - 1.0, 0.0, 1.0), s);
}

// Billboard content: a few looping "ads" picked by id.
vec3 billboard(float id, vec2 uv, float t) {
    float style = floor(h1(id, 1u) * 4.0);
    float hue = h1(id, 2u);
    vec3 c;
    if (style < 1.0) {
        // Gradient sweep with a bright scrolling title bar.
        c = hsv(hue + uv.x * 0.25 + t * 0.03, 0.8, 0.55);
        float bar = step(abs(uv.y - 0.7), 0.08) * step(0.5, fract(uv.x * 6.0 - t * 0.8));
        c += vec3(1.0) * bar * 0.7;
    } else if (style < 2.0) {
        // Concentric rings pulsing out (some music video).
        float r = length(uv - 0.5);
        c = hsv(hue + r * 0.6 - t * 0.1, 0.9, 0.35 + 0.35 * sin(r * 30.0 - t * 4.0));
    } else if (style < 3.0) {
        // "Text" rows of blocks scrolling up.
        float row = floor((uv.y + t * 0.12) * 7.0);
        float col = floor(uv.x * 14.0);
        float on = step(0.45, h2(vec2(row, col), 8u + uint(id) * 977u));
        c = mix(hsv(hue, 0.6, 0.12), hsv(hue + 0.5, 0.5, 0.9), on * step(0.15, fract((uv.y + t * 0.12) * 7.0)));
    } else {
        // Slow color fade between two products, with a flash on the cut.
        float phase = fract(t / 9.0 + h1(id, 3u));
        vec3 a = hsv(hue, 0.7, 0.5);
        vec3 b = hsv(hue + 0.4, 0.7, 0.5);
        c = mix(a, b, step(0.5, phase)) * (0.7 + 0.3 * uv.y);
        c += vec3(exp(-abs(phase - 0.5) * 60.0));
    }
    // Scanlines.
    return c * (0.85 + 0.15 * sin(uv.y * 160.0));
}

// How brightly a light of this kind/id shines right now (1 = as baked).
float light_level(uint kind, float id, float t) {
    if (kind == KIND_WINDOW) {
        // Each window has ~3% chance to be dark in any given ~40 s slot.
        float slot = floor(t / (25.0 + 30.0 * h1(id, 4u)) + h1(id, 5u));
        return step(0.03, h2(vec2(id, slot), 6u));
    }
    if (kind == KIND_SIGN) {
        // Mostly steady; a few signs buzz and stutter now and then.
        float bad = step(0.82, h1(id, 7u));
        float buzz = step(0.9, h2(vec2(id, floor(t * 14.0)), 8u));
        float fail = step(0.985, h2(vec2(id, floor(t * 1.3)), 9u));
        return 1.0 - bad * buzz * 0.7 - fail * 0.85 + 0.06 * sin(t * 3.0 + id);
    }
    if (kind == KIND_BLINKER) {
        float ph = fract(t * 0.6 + h1(id, 10u));
        return smoothstep(0.0, 0.05, ph) * (1.0 - smoothstep(0.18, 0.3, ph)) * 1.4 + 0.08;
    }
    if (kind == KIND_TOWER) {
        return 0.92 + 0.08 * sin(t * 0.8);
    }
    if (kind == KIND_STAR) {
        return 0.6 + 0.4 * sin(t * (1.0 + 2.0 * h1(id, 11u)) + id);
    }
    return 1.0;
}

// The city (everything above the waterline) at a pixel, lights animated.
vec3 city(ivec2 px, float t) {
    vec4 c = texelFetch(color_tex, px, 0);
    uint d = texelFetch(data_tex, px, 0).r;
    uint kind = d & 7u;
    if (kind == 0u) {
        return c.rgb;
    }
    float id = float((d >> 3u) & 0x1fffu);
    if (kind == KIND_BILLBOARD) {
        vec2 uv = vec2(float((d >> 16u) & 255u), float(d >> 24u)) / 255.0;
        return billboard(id, uv, t);
    }
    float lit = light_level(kind, id, t);
    return c.rgb * (1.0 - c.a) + c.rgb * c.a * lit;
}

void main() {
    ivec2 px = ivec2(gl_FragCoord.xy);
    o_color = vec4(city(px, u[0].z) * 0.5, 1.0);
}
