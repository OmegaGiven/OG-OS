// One sakura petal: a heart-tipped ellipse, alpha-blended over the frame.
//
// u[6] layer, cell size, petal radius, -
// u[7] -, -, color scale, alpha scale (depth haze)

in vec2 v_lp;
flat in float v_fw;
flat in float v_flip;

void main() {
    float layer = u[6].x;
    float pr = u[6].z;
    vec2 e = v_lp / vec2(pr * v_fw, pr * 0.62);
    float d = length(e) - 1.0;
    // Notch at the tip (sakura petals are heart-ended).
    d = max(d, -(length(v_lp - vec2(pr * v_fw * 0.95, 0.0)) - pr * 0.22 * v_fw));
    float soft = 0.06 + layer * 0.05;
    float a = 1.0 - smoothstep(-soft, soft, d);
    if (a <= 0.0) {
        discard;
    }
    float shade = 0.65 + 0.35 * abs(v_flip);
    vec3 col = mix(vec3(1.0, 0.62, 0.78), vec3(1.0, 0.86, 0.92), clamp(1.0 - length(e), 0.0, 1.0)) * shade * u[7].z;
    o_color = vec4(u[0].w > 0.5 ? to_srgb(col) : col, a * u[7].w);
}
