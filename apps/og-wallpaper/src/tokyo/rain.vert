// Rain streaks as instanced sprites: one quad per streak, three depth layers
// (one draw each), slanted by the wind. Same layout the old per-pixel rain
// produced: per column x' = x - y * slant, a hashed offset and period, and
// streaks every `period` px falling at the layer's phase.
//
// u[0] w, h, t, -
// u[1] horizon, gust, slant, -
// u[5] fall phase per layer (px, wrapped)
// u[6] layer, first column, streak slots per column, -

out vec2 v_px;   // screen px (top-left origin)
out float v_dx;  // px across the streak, from its centre line
out float v_t;   // 0 at the tail .. 1 at the head

void main() {
    float lf = u[6].x;
    int slots = int(u[6].z);
    float spacing = 16.0 + lf * 12.0;
    float len = 26.0 + lf * 22.0;
    float width = 0.5 + lf * 0.35;
    float slant = u[1].z;
    float col = u[6].y + float(gl_InstanceID / slots);
    float k = float(gl_InstanceID % slots);
    float phase = u[5][int(lf)];
    float h = h2(vec2(col, lf), 14u);
    float cx = (col + 0.2 + 0.6 * h) * spacing;
    float period = len * (3.0 + 5.0 * h2(vec2(col, lf), 15u));
    float top = mod(phase + h * 997.0, period) - period + k * period;
    if (top > u[0].y) {
        gl_Position = vec4(2.0, 2.0, 2.0, 1.0); // unused slot: off screen
        return;
    }
    float sx = float(gl_VertexID & 1) * 2.0 - 1.0;
    float sy = float((gl_VertexID >> 1) & 1);
    float hw = width + 1.3;
    float y = top + sy * len;
    v_px = vec2(cx + sx * hw + y * slant, y);
    v_dx = sx * hw;
    v_t = sy;
    gl_Position = clip(v_px);
}
