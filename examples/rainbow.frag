// Sample Shadertoy-compatible GLSL fragment shader for hyprwpe
// Renders dynamic animated fluid plasma waves

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = (fragCoord.xy - 0.5 * iResolution.xy) / min(iResolution.x, iResolution.y);
    float t = iTime * 0.8;

    vec3 col = vec3(0.0);
    for (float i = 1.0; i < 4.0; i++) {
        uv.x += 0.3 / i * sin(i * 3.0 * uv.y + t + 0.3 * i);
        uv.y += 0.3 / i * cos(i * 3.0 * uv.x + t + 0.3 * (i + 10.0));
    }

    float r = sin(uv.x + uv.y + 1.0) * 0.5 + 0.5;
    float g = sin(uv.x + uv.y + 2.0 + t * 0.2) * 0.5 + 0.5;
    float b = sin(uv.x + uv.y + 4.0 - t * 0.2) * 0.5 + 0.5;

    fragColor = vec4(r, g, b, 1.0);
}
