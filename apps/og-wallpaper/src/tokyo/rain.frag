// Rain streak, added over the frame; lit a little by what's behind it.
//
// u[1] horizon, -, -, -
// u[6] layer, -, -, -

uniform mediump sampler2D cache;

in vec2 v_px;
in float v_dx;
in float v_t;

void main() {
    float lf = u[6].x;
    float width = 0.5 + lf * 0.35;
    float a = v_t * (1.0 - smoothstep(width, width + 0.8, abs(v_dx))) * (0.05 + lf * 0.035);
    // Brightness behind: the cached city, or its dimmer reflection.
    float horizon = u[1].x;
    float y = v_px.y < horizon ? v_px.y : horizon - (v_px.y - horizon) * 0.92 - 4.0;
    ivec2 p = ivec2(clamp(v_px.x, 0.0, u[0].x - 1.0), clamp(y, 0.0, horizon - 6.0));
    vec3 behind = texelFetch(cache, p, 0).rgb * (v_px.y < horizon ? 2.0 : 0.7);
    float lum = dot(behind, vec3(0.3, 0.5, 0.2));
    vec3 c = vec3(0.62, 0.68, 0.85) * a * (0.3 + lum * 2.0);
    o_color = vec4(u[0].w > 0.5 ? to_srgb(c) : c, 1.0);
}
