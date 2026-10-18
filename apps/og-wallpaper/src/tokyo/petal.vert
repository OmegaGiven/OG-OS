// Sakura petals as instanced sprites: one quad per grid cell of a depth
// layer (one draw each); ~58% of cells are empty and collapse. Each petal
// drifts with the layer offset, flutters, tumbles, and is pushed away from
// the cursor.
//
// u[0] w, h, t, -
// u[2] mouse x, y, present, -
// u[3] layer 0 offset xy, layer 1 offset xy
// u[4] layer 2 offset xy, -, -
// u[6] layer, cell size, petal radius, cells per row

out vec2 v_lp;          // px in the petal's own rotated frame
flat out float v_fw;    // width while tumbling (edge-on -> thin)
flat out float v_flip;

void main() {
    float layer = u[6].x;
    float cs = u[6].y;
    float pr = u[6].z;
    int nx = int(u[6].w);
    float t = u[0].z;
    vec2 off = layer < 0.5 ? u[3].xy : (layer < 1.5 ? u[3].zw : u[4].xy);
    // Cells covering the screen plus a 2-cell margin (flutter, cursor push).
    vec2 cell = floor(-off / cs) - 2.0 + vec2(float(gl_InstanceID % nx), float(gl_InstanceID / nx));
    // Ids repeat every 1024 cells (the drift offsets wrap too).
    vec2 key = mod(cell, 1024.0) + layer * 31.0;
    vec2 h = h22(key, 16u);
    if (h.x > 0.42) {
        gl_Position = vec4(2.0, 2.0, 2.0, 1.0); // empty cell
        return;
    }
    vec2 hb = h22(key, 17u);
    vec2 c = (cell + 0.25 + 0.5 * hb) * cs + off;
    // Flutter: sideways sway and a little bob.
    c += vec2(sin(t * (0.7 + h.y) + hb.x * 6.28) * cs * 0.12, cos(t * (0.9 + hb.y) + h.y * 6.28) * cs * 0.06);
    if (u[2].z > 0.5) {
        vec2 dv = c - u[2].xy;
        float dist = max(length(dv), 1.0);
        c += dv / dist * cs * 0.9 * exp(-dist / (cs * 0.9));
    }
    float ang = hb.x * 6.28 + t * (0.6 + h.y * 1.4) * (hb.y > 0.5 ? -1.0 : 1.0);
    float ca = cos(ang);
    float sa = sin(ang);
    // Tumbling: the petal turns edge-on periodically.
    float flip = cos(t * (1.2 + h.x * 2.0) + hb.y * 6.28);
    float fw = max(abs(flip), 0.18);
    vec2 corner = vec2(float(gl_VertexID & 1) * 2.0 - 1.0, float((gl_VertexID >> 1) & 1) * 2.0 - 1.0);
    v_lp = corner * vec2(pr * fw, pr * 0.62) * 1.25;
    v_fw = fw;
    v_flip = flip;
    gl_Position = clip(c + vec2(ca * v_lp.x - sa * v_lp.y, sa * v_lp.x + ca * v_lp.y));
}
