//! Fragment shader wallpapers rendered through OpenGL ES.
//!
//! Executes standalone GLSL shaders (`.glsl` / `.frag`) directly on the EGL background
//! surface. Supports standard GLSL shaders and Shadertoy-style shaders with
//! common uniforms (`iResolution`, `iTime`, `iTimeDelta`, `iFrameRate`, `iFrame`,
//! `iMouse`, `iDate`).
//!
//! Like the video renderer, frame callbacks drive presentation: when all outputs
//! are occluded or when suspended, frame callbacks stop and the GPU burns 0% CPU.

use anyhow::{bail, Context, Result};
use glow::HasContext;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERTEX_SHADER_SOURCE: &str = r#"#version 300 es
layout(location = 0) in vec2 position;
void main() {
    gl_Position = vec4(position, 0.0, 1.0);
}
"#;

const FULLSCREEN_VERTICES: [f32; 8] = [
    -1.0, -1.0,
     1.0, -1.0,
    -1.0,  1.0,
     1.0,  1.0,
];

/// Prepares a user-provided fragment shader for compilation on OpenGL ES 3.0.
///
/// Automatically handles:
/// - Version injection (`#version 300 es`) if missing.
/// - Precision specifications for float and int.
/// - Shadertoy `mainImage(out vec4, in vec2)` wrapping to `void main()`.
/// - Compatibility macro `texture2D -> texture`.
/// - Preamble with standard uniforms (`iResolution`, `iTime`, `iTimeDelta`, etc.).
pub fn prepare_fragment_shader(source: &str) -> String {
    let is_shadertoy = source.contains("mainImage");
    let has_version = source.lines().any(|l| l.trim_start().starts_with("#version"));
    let has_gl_frag_color = source.contains("gl_FragColor");

    let mut output = String::new();

    if !has_version {
        output.push_str("#version 300 es\n");
    }

    output.push_str(
        r#"#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
precision highp int;
#else
precision mediump float;
precision mediump int;
#endif

#define texture2D texture

"#,
    );

    // Uniform declarations if not already declared in the source
    if !source.contains("uniform") || !source.contains("iResolution") {
        output.push_str("uniform vec3 iResolution;\n");
    }
    if !source.contains("uniform") || !source.contains("iTime") {
        output.push_str("uniform float iTime;\n");
    }
    if !source.contains("uniform") || !source.contains("iTimeDelta") {
        output.push_str("uniform float iTimeDelta;\n");
    }
    if !source.contains("uniform") || !source.contains("iFrameRate") {
        output.push_str("uniform float iFrameRate;\n");
    }
    if !source.contains("uniform") || !source.contains("iFrame") {
        output.push_str("uniform int iFrame;\n");
    }
    if !source.contains("uniform") || !source.contains("iMouse") {
        output.push_str("uniform vec4 iMouse;\n");
    }
    if !source.contains("uniform") || !source.contains("iDate") {
        output.push_str("uniform vec4 iDate;\n");
    }

    if is_shadertoy {
        output.push_str("\nout vec4 hyprwpe_FragColor;\n\n");
        output.push_str(source);
        output.push_str(
            r#"

void main() {
    vec4 col = vec4(0.0, 0.0, 0.0, 1.0);
    mainImage(col, gl_FragCoord.xy);
    hyprwpe_FragColor = col;
}
"#,
        );
    } else if has_gl_frag_color {
        output.push_str("\nout vec4 hyprwpe_FragColor;\n");
        output.push_str("#define gl_FragColor hyprwpe_FragColor\n\n");
        output.push_str(source);
    } else {
        // Standard shader without mainImage or gl_FragColor
        if !source.contains("out vec4") {
            output.push_str("\nout vec4 fragColor;\n\n");
        }
        output.push_str(source);
    }

    output
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y } else { y + 1 };
    (y as i32, m, d)
}

/// Returns current date formatted for Shadertoy `iDate` uniform:
/// `vec4(year, month - 1, day, seconds_since_midnight)`
fn current_date_uniform() -> (f32, f32, f32, f32) {
    let now = SystemTime::now();
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_secs = duration.as_secs();
    let days = (total_secs / 86400) as i64;
    let (year, month, day) = days_to_ymd(days);
    let sec_of_day = (total_secs % 86400) as f32 + (duration.subsec_nanos() as f32 / 1_000_000_000.0);
    (year as f32, (month - 1) as f32, day as f32, sec_of_day)
}

/// A compiled GLSL shader renderer bound to an output.
pub struct ShaderPlayer {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    loc_resolution: Option<glow::UniformLocation>,
    loc_time: Option<glow::UniformLocation>,
    loc_time_delta: Option<glow::UniformLocation>,
    loc_frame_rate: Option<glow::UniformLocation>,
    loc_frame: Option<glow::UniformLocation>,
    loc_mouse: Option<glow::UniformLocation>,
    loc_date: Option<glow::UniformLocation>,
    start_time: Instant,
    last_frame_time: Instant,
    paused_duration: Duration,
    pause_start: Option<Instant>,
    frame_count: i32,
    is_paused: bool,
}

impl ShaderPlayer {
    /// Load, preprocess, compile, and link a fragment shader from `path`.
    pub fn new(path: &Path, gl: &glow::Context) -> Result<Self> {
        let raw_source = std::fs::read_to_string(path)
            .with_context(|| format!("reading shader file {}", path.display()))?;
        let processed_source = prepare_fragment_shader(&raw_source);

        unsafe {
            let vs = compile_shader(gl, glow::VERTEX_SHADER, VERTEX_SHADER_SOURCE)
                .context("compiling vertex shader")?;
            let fs = match compile_shader(gl, glow::FRAGMENT_SHADER, &processed_source) {
                Ok(fs) => fs,
                Err(e) => {
                    gl.delete_shader(vs);
                    return Err(e).with_context(|| format!("compiling {}", path.display()));
                }
            };

            let program = match link_program(gl, vs, fs) {
                Ok(p) => p,
                Err(e) => {
                    gl.delete_shader(vs);
                    gl.delete_shader(fs);
                    return Err(e).with_context(|| format!("linking {}", path.display()));
                }
            };

            // Detach and delete individual shader stages; program retains linked code.
            gl.delete_shader(vs);
            gl.delete_shader(fs);

            // Locate uniform variables
            let loc_resolution = gl.get_uniform_location(program, "iResolution");
            let loc_time = gl.get_uniform_location(program, "iTime");
            let loc_time_delta = gl.get_uniform_location(program, "iTimeDelta");
            let loc_frame_rate = gl.get_uniform_location(program, "iFrameRate");
            let loc_frame = gl.get_uniform_location(program, "iFrame");
            let loc_mouse = gl.get_uniform_location(program, "iMouse");
            let loc_date = gl.get_uniform_location(program, "iDate");

            // Setup fullscreen quad geometry
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("failed to create VAO: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("failed to create VBO: {e}"))?;

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));

            let byte_slice = std::slice::from_raw_parts(
                FULLSCREEN_VERTICES.as_ptr() as *const u8,
                FULLSCREEN_VERTICES.len() * std::mem::size_of::<f32>(),
            );
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, byte_slice, glow::STATIC_DRAW);

            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(
                0,
                2,
                glow::FLOAT,
                false,
                (2 * std::mem::size_of::<f32>()) as i32,
                0,
            );

            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_vertex_array(None);

            let now = Instant::now();
            Ok(ShaderPlayer {
                program,
                vao,
                vbo,
                loc_resolution,
                loc_time,
                loc_time_delta,
                loc_frame_rate,
                loc_frame,
                loc_mouse,
                loc_date,
                start_time: now,
                last_frame_time: now,
                paused_duration: Duration::ZERO,
                pause_start: None,
                frame_count: 0,
                is_paused: false,
            })
        }
    }

    /// Pause or resume the internal clock.
    ///
    /// When paused, animation time does not advance, ensuring seamless continuation
    /// upon unpausing.
    pub fn set_paused(&mut self, paused: bool) {
        if self.is_paused == paused {
            return;
        }
        self.is_paused = paused;
        if paused {
            self.pause_start = Some(Instant::now());
        } else if let Some(start) = self.pause_start.take() {
            self.paused_duration += start.elapsed();
            self.last_frame_time = Instant::now();
        }
    }

    /// Render one frame covering the full viewport.
    pub fn render(&mut self, gl: &glow::Context, width: i32, height: i32) -> Result<()> {
        let now = Instant::now();
        let elapsed = if let Some(pause_start) = self.pause_start {
            pause_start
                .saturating_duration_since(self.start_time)
                .saturating_sub(self.paused_duration)
        } else {
            now.saturating_duration_since(self.start_time)
                .saturating_sub(self.paused_duration)
        };
        let time_sec = elapsed.as_secs_f32();
        let delta_sec = now
            .saturating_duration_since(self.last_frame_time)
            .as_secs_f32();
        self.last_frame_time = now;
        self.frame_count = self.frame_count.wrapping_add(1);

        unsafe {
            gl.viewport(0, 0, width, height);
            gl.use_program(Some(self.program));

            if let Some(loc) = &self.loc_resolution {
                gl.uniform_3_f32(Some(loc), width as f32, height as f32, 1.0);
            }
            if let Some(loc) = &self.loc_time {
                gl.uniform_1_f32(Some(loc), time_sec);
            }
            if let Some(loc) = &self.loc_time_delta {
                gl.uniform_1_f32(Some(loc), delta_sec);
            }
            if let Some(loc) = &self.loc_frame_rate {
                let fps = if delta_sec > 0.0001 {
                    1.0 / delta_sec
                } else {
                    60.0
                };
                gl.uniform_1_f32(Some(loc), fps);
            }
            if let Some(loc) = &self.loc_frame {
                gl.uniform_1_i32(Some(loc), self.frame_count);
            }
            if let Some(loc) = &self.loc_mouse {
                gl.uniform_4_f32(Some(loc), 0.0, 0.0, 0.0, 0.0);
            }
            if let Some(loc) = &self.loc_date {
                let (year, month, day, sec) = current_date_uniform();
                gl.uniform_4_f32(Some(loc), year, month, day, sec);
            }

            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
        Ok(())
    }

    /// Delete GPU objects allocated for this shader.
    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
        }
    }
}

unsafe fn compile_shader(
    gl: &glow::Context,
    shader_type: u32,
    source: &str,
) -> Result<glow::Shader> {
    let shader = gl
        .create_shader(shader_type)
        .map_err(|e| anyhow::anyhow!("failed to create shader: {e}"))?;
    gl.shader_source(shader, source);
    gl.compile_shader(shader);
    if !gl.get_shader_compile_status(shader) {
        let log = gl.get_shader_info_log(shader);
        gl.delete_shader(shader);
        bail!("shader compilation failed:\n{log}");
    }
    Ok(shader)
}

unsafe fn link_program(
    gl: &glow::Context,
    vs: glow::Shader,
    fs: glow::Shader,
) -> Result<glow::Program> {
    let program = gl
        .create_program()
        .map_err(|e| anyhow::anyhow!("failed to create program: {e}"))?;
    gl.attach_shader(program, vs);
    gl.attach_shader(program, fs);
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        gl.detach_shader(program, vs);
        gl.detach_shader(program, fs);
        gl.delete_program(program);
        bail!("program linking failed:\n{log}");
    }
    gl.detach_shader(program, vs);
    gl.detach_shader(program, fs);
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_shadertoy_shader() {
        let source = r#"
void mainImage( out vec4 fragColor, in vec2 fragCoord ) {
    vec2 uv = fragCoord / iResolution.xy;
    fragColor = vec4(uv, 0.5 + 0.5 * sin(iTime), 1.0);
}
"#;
        let processed = prepare_fragment_shader(source);
        assert!(processed.contains("#version 300 es"));
        assert!(processed.contains("precision highp float;"));
        assert!(processed.contains("uniform vec3 iResolution;"));
        assert!(processed.contains("uniform float iTime;"));
        assert!(processed.contains("out vec4 hyprwpe_FragColor;"));
        assert!(processed.contains("void main() {"));
        assert!(processed.contains("mainImage(col, gl_FragCoord.xy);"));
        assert!(processed.contains("hyprwpe_FragColor = col;"));
    }

    #[test]
    fn preprocess_standard_glsl_shader() {
        let source = r#"
out vec4 color;
void main() {
    color = vec4(1.0, 0.0, 0.0, 1.0);
}
"#;
        let processed = prepare_fragment_shader(source);
        assert!(processed.contains("#version 300 es"));
        assert!(processed.contains("uniform float iTime;"));
        assert!(!processed.contains("mainImage"));
        assert!(processed.contains("out vec4 color;"));
    }

    #[test]
    fn preprocess_legacy_gl_frag_color() {
        let source = r#"
void main() {
    gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0);
}
"#;
        let processed = prepare_fragment_shader(source);
        assert!(processed.contains("#version 300 es"));
        assert!(processed.contains("#define gl_FragColor hyprwpe_FragColor"));
        assert!(processed.contains("out vec4 hyprwpe_FragColor;"));
    }

    #[test]
    fn date_uniform_calculation() {
        let (y, m, d, sec) = current_date_uniform();
        assert!(y >= 2024.0);
        assert!(m >= 0.0 && m <= 11.0);
        assert!(d >= 1.0 && d <= 31.0);
        assert!(sec >= 0.0 && sec < 86400.0);
    }
}
