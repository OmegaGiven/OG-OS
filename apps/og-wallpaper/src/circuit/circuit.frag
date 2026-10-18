// Circuit wallpaper: static board + per-pixel trace data -> moving pulses,
// plus the CPU's neon omega and its cursor-following "eye".
//
// u[0] w, h, t (s), 1 = shader must sRGB-encode
// u[1] accent (linear rgb), breathe 0..1
// u[2] hot (linear rgb), flicker
// u[3] cpu centre x, y, die half-size, omega radius
// u[4] eye x, y, blink (0 open..1 shut), open factor
// u[5] vignette aspect, -, -, -

uniform mediump sampler2D board; // R8: bg->accent blend factor
uniform mediump sampler2D lut;   // 256x1 sRGB: bg->accent ramp (samples linear)
uniform highp usampler2D data;   // packed trace info (see board.rs `pack`)

const float HALO = 11.0;

// Signed distance to an arc of radius r centred on the origin, opening facing
// +y (down on screen), with half-aperture `gap` radians around the bottom.
float sd_arc(vec2 p, float r, float gap) {
    float a = atan(p.x, -p.y); // 0 at top, +-pi at bottom
    float half_span = 3.14159265 - gap;
    if (abs(a) <= half_span) {
        return abs(length(p) - r);
    }
    vec2 end = vec2(sin(half_span) * sign(p.x), -cos(half_span)) * r;
    return length(p - end);
}

float sd_seg(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

// The OG logo omega: ring open at the bottom, each end continuing
// outward-and-down at 45 degrees.
float sd_omega(vec2 p, float r) {
    const float gap = 0.66;
    float d = sd_arc(p, r, gap);
    float ex = sin(gap) * r;
    float ey = cos(gap) * r;
    float leg = r * 0.48 * 0.70710678;
    d = min(d, sd_seg(p, vec2(-ex, ey), vec2(-ex - leg, ey + leg)));
    d = min(d, sd_seg(p, vec2(ex, ey), vec2(ex + leg, ey + leg)));
    return d;
}

vec2 pulse(float along, uint seed_u, float seed, float bus, bool filler, float t) {
    float speed = 320.0 + 260.0 * bus;
    float period = 380.0 + 620.0 * fract(bus * 7.13);
    float trail = 22.0 + 26.0 * fract(bus * 3.7);
    float density = 0.5;
    float gain = 1.0;
    if (filler) {
        speed = 110.0 + 90.0 * bus;
        period = 700.0 + 900.0 * fract(seed * 5.3);
        trail = 18.0;
        density = 0.2;
        gain = 0.5;
    }
    // Direction is per bus (all its traces agree): ~60% carry pulses in to
    // the CPU, the rest out. `along` is measured from the CPU end.
    float dir = fract(bus * 17.31) < 0.4 ? -1.0 : 1.0;
    float q = (along * dir + speed * t) / period + bus * 13.0;
    float packet = floor(q);
    float x = fract(q) * period; // px behind the head
    // Each trace in a bus decides per packet whether its "bit" is set.
    float bit = step(1.0 - density, unorm(hu(uint(int(packet)) * 0x9e3779b1u + seed_u))) * gain;
    float edge = smoothstep(0.0, 2.5, x); // crisp leading edge
    return vec2(bit * edge * exp(-x / 5.0), bit * edge * exp(-x / trail));
}

void main() {
    vec2 frag = vec2(gl_FragCoord.x, u[0].y - gl_FragCoord.y);
    ivec2 px = ivec2(frag);
    float t = u[0].z;
    float k = texelFetch(board, px, 0).r;
    vec3 col = texelFetch(lut, ivec2(int(k * 255.0 + 0.5), 0), 0).rgb;
    uint pk = texelFetch(data, px, 0).r;
    uint dist_q = (pk >> 13u) & 63u;

    vec3 accent = u[1].rgb;
    vec3 hot = u[2].rgb;

    // --- trace pulses ---
    if (dist_q != 63u) {
        float along = float(pk & 0x1fffu);
        float dz = float(dist_q) * (HALO / 62.0);
        uint seed_u = (pk >> 19u) & 63u;
        float seed = (float(seed_u) + 0.5) / 64.0;
        float bus = (float((pk >> 25u) & 63u) + 0.5) / 64.0;
        bool filler = (pk >> 31u) == 1u;
        vec2 p = pulse(along, seed_u, seed, bus, filler, t);
        float core = 1.0 - smoothstep(0.6, 1.9, dz);
        float glow = exp(-dz * dz / 18.0);
        float spark = exp(-dz * dz / 40.0);
        col += vec3(1.0, 0.96, 0.88) * p.x * (core * 2.2 + spark * 0.9)
             + hot * p.y * core * 1.3
             + accent * p.y * glow * 0.5;
        // Faint idle shimmer so quiet traces still read as "live".
        col += accent * core * 0.025 * (0.5 + 0.5 * sin(t * 1.3 + seed * 40.0));
    }

    // --- CPU: breathing glow, neon omega, eye (only near the die) ---
    vec2 rel = frag - u[3].xy;
    float die_half = u[3].z;
    float r = u[3].w;
    if (abs(rel.x) < die_half * 1.6 && abs(rel.y) < die_half * 1.6) {
        float in_die = step(abs(rel.x), die_half) * step(abs(rel.y), die_half);
        col += accent * in_die * (0.02 + 0.03 * u[1].w) * exp(-length(rel) / (die_half * 0.9));

        vec2 op = rel - vec2(0.0, -r * 0.12);
        float od = sd_omega(op, r);
        float ring_w = r * 0.085;
        float ring = 1.0 - smoothstep(ring_w - 1.2, ring_w + 0.8, od);
        float ring_halo = exp(-max(od - ring_w, 0.0) / (r * 0.16));
        col += (hot * ring * 1.15 + accent * ring_halo * 0.40) * u[2].w;

        // Eye: a dot of light circled by a ring, following the cursor.
        vec2 q = frag - u[4].xy;
        q.y = q.y / max(1.0 - u[4].z * 0.92, 0.08); // squash while blinking
        float er = length(q);
        float iris_r = r * 0.30;
        float iris_w = r * 0.035;
        float id = abs(er - iris_r);
        float iris = 1.0 - smoothstep(iris_w - 1.0, iris_w + 0.8, id);
        float iris_glow = exp(-max(id - iris_w, 0.0) / (r * 0.07));
        float fill = (1.0 - smoothstep(iris_r - iris_w, iris_r, er)) * exp(-er / (iris_r * 0.6));
        float dot_r = r * 0.11;
        float pupil = 1.0 - smoothstep(dot_r * 0.55, dot_r, er);
        float pupil_glow = exp(-er / (dot_r * 1.6)) * 0.8;
        col += (hot * iris * 1.1 + accent * iris_glow * 0.35 + accent * fill * 0.18
                + vec3(1.0, 0.97, 0.9) * pupil * 1.4 + hot * pupil_glow) * u[4].w;
    }

    // Vignette.
    vec2 uv = frag / u[0].xy;
    float vig = 1.0 - smoothstep(0.35, 1.25, length((uv - 0.5) * vec2(1.0, u[5].x)));
    col *= mix(0.55, 1.0, vig);

    finish(col);
}
