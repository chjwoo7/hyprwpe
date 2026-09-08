//! Wallpaper Engine `scene.pkg` scene renderer.
//!
//! Loads `scene.pkg` containers, parses `scene.json` scene graphs, extracts and
//! decompresses textures (.tex / images), and renders 2D transformed layers
//! using OpenGL ES 3.0.

use anyhow::{bail, Context, Result};
use glow::HasContext;
use hyprwpe_core::pkg::Package;
use hyprwpe_core::scene::{ObjectKind, Scene};
use hyprwpe_core::tex::TexImage;
use std::path::Path;

const VERTEX_SHADER_SOURCE: &str = r#"#version 300 es
layout(location = 0) in vec2 aPosition;
layout(location = 1) in vec2 aTexCoord;

uniform mat4 uProjection;
uniform mat4 uModel;

out vec2 vTexCoord;

void main() {
    vTexCoord = aTexCoord;
    gl_Position = uProjection * uModel * vec4(aPosition, 0.0, 1.0);
}
"#;

const FRAGMENT_SHADER_SOURCE: &str = r#"#version 300 es
precision mediump float;

in vec2 vTexCoord;
uniform sampler2D uTexture;
uniform vec4 uColor;

out vec4 fragColor;

void main() {
    vec4 tex = texture(uTexture, vTexCoord);
    fragColor = tex * uColor;
}
"#;

/// 2D Quad vertices: [posX, posY, texU, texV]
const QUAD_VERTICES: [f32; 16] = [
    // Triangle 1
    0.0, 0.0, 0.0, 1.0,
    1.0, 0.0, 1.0, 1.0,
    0.0, 1.0, 0.0, 0.0,
    1.0, 1.0, 1.0, 0.0,
];

/// Simple 4x4 matrix for 2D orthographic projection and layer transforms.
#[derive(Debug, Clone, Copy)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub fn identity() -> Self {
        Mat4([
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ])
    }

    pub fn ortho(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Self {
        let r_l = right - left;
        let t_b = top - bottom;
        let f_n = far - near;

        let mut m = Self::identity();
        if r_l != 0.0 && t_b != 0.0 && f_n != 0.0 {
            m.0[0] = 2.0 / r_l;
            m.0[5] = 2.0 / t_b;
            m.0[10] = -2.0 / f_n;
            m.0[12] = -(right + left) / r_l;
            m.0[13] = -(top + bottom) / t_b;
            m.0[14] = -(far + near) / f_n;
        }
        m
    }

    pub fn mul(&self, other: &Mat4) -> Mat4 {
        let mut res = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += self.0[k * 4 + row] * other.0[col * 4 + k];
                }
                res[col * 4 + row] = sum;
            }
        }
        Mat4(res)
    }

    pub fn translate(x: f32, y: f32, z: f32) -> Self {
        let mut m = Self::identity();
        m.0[12] = x;
        m.0[13] = y;
        m.0[14] = z;
        m
    }

    pub fn scale(sx: f32, sy: f32, sz: f32) -> Self {
        let mut m = Self::identity();
        m.0[0] = sx;
        m.0[5] = sy;
        m.0[10] = sz;
        m
    }

    pub fn rotate_z(rad: f32) -> Self {
        let mut m = Self::identity();
        let c = rad.cos();
        let s = rad.sin();
        m.0[0] = c;
        m.0[1] = s;
        m.0[4] = -s;
        m.0[5] = c;
        m
    }
}

/// A rendered 2D image layer.
pub struct RenderLayer {
    pub texture: glow::Texture,
    pub width: f32,
    pub height: f32,
    pub origin: [f32; 3],
    pub scale: [f32; 3],
    pub angles: [f32; 3],
    pub color: [f32; 4],
    pub visible: bool,
}

/// A scene player rendering a Wallpaper Engine scene package.
pub struct ScenePlayer {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    loc_projection: Option<glow::UniformLocation>,
    loc_model: Option<glow::UniformLocation>,
    loc_color: Option<glow::UniformLocation>,
    layers: Vec<RenderLayer>,
    is_paused: bool,
}

impl ScenePlayer {
    /// Load a scene from a `scene.pkg` file or directory containing it.
    pub fn new(path: &Path, gl: &glow::Context) -> Result<Self> {
        let pkg_path = if path.is_dir() {
            path.join("scene.pkg")
        } else {
            path.to_path_buf()
        };

        let pkg = Package::open(&pkg_path)
            .with_context(|| format!("opening scene package {}", pkg_path.display()))?;

        let scene_json = pkg
            .get_str("scene.json")
            .context("scene.pkg contains no scene.json")?;
        let scene = Scene::from_json_str(scene_json)
            .context("parsing scene.json from package")?;

        unsafe {
            let vs = compile_shader(gl, glow::VERTEX_SHADER, VERTEX_SHADER_SOURCE)?;
            let fs = compile_shader(gl, glow::FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE)?;
            let program = link_program(gl, vs, fs)?;
            gl.delete_shader(vs);
            gl.delete_shader(fs);

            let loc_projection = gl.get_uniform_location(program, "uProjection");
            let loc_model = gl.get_uniform_location(program, "uModel");
            let loc_color = gl.get_uniform_location(program, "uColor");

            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("failed to create VAO: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("failed to create VBO: {e}"))?;

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));

            let byte_slice = std::slice::from_raw_parts(
                QUAD_VERTICES.as_ptr() as *const u8,
                QUAD_VERTICES.len() * std::mem::size_of::<f32>(),
            );
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, byte_slice, glow::STATIC_DRAW);

            let stride = (4 * std::mem::size_of::<f32>()) as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);

            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(
                1,
                2,
                glow::FLOAT,
                false,
                stride,
                (2 * std::mem::size_of::<f32>()) as i32,
            );

            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_vertex_array(None);

            let mut layers = Vec::new();

            for obj in &scene.objects {
                if obj.kind() != ObjectKind::Image {
                    continue;
                }
                let Some(mat_path) = obj.image_path() else {
                    continue;
                };

                let texture_file = resolve_texture_file(&pkg, &mat_path);
                let Some(tex_name) = texture_file else {
                    continue;
                };

                if let Some((tex_handle, tw, th)) = load_texture_from_pkg(&pkg, &tex_name, gl) {
                    let mut size = [tw as f32, th as f32];
                    if let Some(s) = obj.size() {
                        if s[0] > 0.0 && s[1] > 0.0 {
                            size = s;
                        }
                    }

                    let color_rgb = obj.color.map(|c| [c.x(), c.y(), c.z()]).unwrap_or([1.0, 1.0, 1.0]);
                    let alpha = obj.alpha();
                    let color = [color_rgb[0], color_rgb[1], color_rgb[2], alpha];

                    layers.push(RenderLayer {
                        texture: tex_handle,
                        width: size[0],
                        height: size[1],
                        origin: obj.origin(),
                        scale: obj.scale(),
                        angles: obj.angles(),
                        color,
                        visible: obj.is_visible(),
                    });
                }
            }

            Ok(ScenePlayer {
                program,
                vao,
                vbo,
                loc_projection,
                loc_model,
                loc_color,
                layers,
                is_paused: false,
            })
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.is_paused = paused;
    }

    /// Render scene layers onto the viewport.
    pub fn render(&mut self, gl: &glow::Context, width: i32, height: i32) -> Result<()> {
        if self.is_paused || width <= 0 || height <= 0 {
            return Ok(());
        }

        unsafe {
            gl.viewport(0, 0, width, height);

            // Enable standard 2D alpha blending
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);

            gl.use_program(Some(self.program));

            // Orthographic projection: (0, 0) bottom-left, (width, height) top-right
            let proj = Mat4::ortho(0.0, width as f32, 0.0, height as f32, -1000.0, 1000.0);
            if let Some(loc) = &self.loc_projection {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &proj.0);
            }

            gl.bind_vertex_array(Some(self.vao));

            for layer in &self.layers {
                if !layer.visible {
                    continue;
                }

                // Compute layer model matrix:
                // Translate to origin -> rotate -> scale to size
                let t = Mat4::translate(layer.origin[0], layer.origin[1], layer.origin[2]);
                let r = Mat4::rotate_z(layer.angles[2].to_radians());
                let s = Mat4::scale(layer.width * layer.scale[0], layer.height * layer.scale[1], 1.0);

                let model = t.mul(&r).mul(&s);

                if let Some(loc) = &self.loc_model {
                    gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.0);
                }
                if let Some(loc) = &self.loc_color {
                    gl.uniform_4_f32(
                        Some(loc),
                        layer.color[0],
                        layer.color[1],
                        layer.color[2],
                        layer.color[3],
                    );
                }

                gl.bind_texture(glow::TEXTURE_2D, Some(layer.texture));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }

            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::BLEND);
        }

        Ok(())
    }

    /// Free allocated textures, program, and buffers.
    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            for layer in self.layers {
                gl.delete_texture(layer.texture);
            }
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
        }
    }
}

/// Resolves a texture file path from a material reference.
fn resolve_texture_file(pkg: &Package, mat_path: &str) -> Option<String> {
    if mat_path.ends_with(".tex")
        || mat_path.ends_with(".png")
        || mat_path.ends_with(".jpg")
        || mat_path.ends_with(".jpeg")
    {
        return Some(mat_path.to_string());
    }

    if let Some(mat_str) = pkg.get_str(mat_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(mat_str) {
            // Check passes[0].textures[0]
            if let Some(arr) = json.get("passes").and_then(|p| p.get(0)).and_then(|p0| p0.get("textures")).and_then(|t| t.as_array()) {
                if let Some(tex) = arr.first().and_then(|v| v.as_str()) {
                    return Some(tex.to_string());
                }
            }
            // Check textures[0]
            if let Some(arr) = json.get("textures").and_then(|t| t.as_array()) {
                if let Some(tex) = arr.first().and_then(|v| v.as_str()) {
                    return Some(tex.to_string());
                }
            }
        }
    }

    // Try fallback replacing .json with .tex
    let tex_candidate = mat_path.replace(".json", ".tex");
    if pkg.get(&tex_candidate).is_some() {
        return Some(tex_candidate);
    }

    None
}

/// Load and upload texture data from package to GPU.
unsafe fn load_texture_from_pkg(
    pkg: &Package,
    path: &str,
    gl: &glow::Context,
) -> Option<(glow::Texture, u32, u32)> {
    let raw = pkg.get(path)?;

    let (pixels, width, height) = if path.ends_with(".tex") {
        let tex = TexImage::parse(raw).ok()?;
        let rgba = tex.to_rgba8().ok()?;
        (rgba, tex.width, tex.height)
    } else {
        let img = image::load_from_memory(raw).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        (img.into_raw(), w, h)
    };

    let texture = gl.create_texture().ok()?;
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));

    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA as i32,
        width as i32,
        height as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(&pixels)),
    );

    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);

    gl.bind_texture(glow::TEXTURE_2D, None);

    Some((texture, width, height))
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
    fn mat4_identity_and_multiplication() {
        let id = Mat4::identity();
        let t = Mat4::translate(10.0, 20.0, 0.0);
        let res = id.mul(&t);
        assert_eq!(res.0[12], 10.0);
        assert_eq!(res.0[13], 20.0);
    }

    #[test]
    fn mat4_ortho_projection() {
        let ortho = Mat4::ortho(0.0, 1920.0, 0.0, 1080.0, -1.0, 1.0);
        assert_eq!(ortho.0[0], 2.0 / 1920.0);
        assert_eq!(ortho.0[5], 2.0 / 1080.0);
        assert_eq!(ortho.0[12], -1.0);
        assert_eq!(ortho.0[13], -1.0);
    }

    #[test]
    fn resolve_texture_file_variations() {
        // 1. Direct extensions
        let pkg = make_test_pkg(&[]);
        assert_eq!(
            resolve_texture_file(&pkg, "textures/bg.tex").as_deref(),
            Some("textures/bg.tex")
        );
        assert_eq!(
            resolve_texture_file(&pkg, "textures/bg.png").as_deref(),
            Some("textures/bg.png")
        );

        // 2. Material with passes.textures
        let mat_json = r#"{"passes": [{"textures": ["textures/forest.tex"]}]}"#;
        let pkg2 = make_test_pkg(&[("materials/forest.json", mat_json.as_bytes())]);
        assert_eq!(
            resolve_texture_file(&pkg2, "materials/forest.json").as_deref(),
            Some("textures/forest.tex")
        );

        // 3. Material with top-level textures
        let mat_json3 = r#"{"textures": ["textures/sky.tex"]}"#;
        let pkg3 = make_test_pkg(&[("materials/sky.json", mat_json3.as_bytes())]);
        assert_eq!(
            resolve_texture_file(&pkg3, "materials/sky.json").as_deref(),
            Some("textures/sky.tex")
        );

        // 4. Fallback .json -> .tex when present
        let pkg4 = make_test_pkg(&[("materials/water.tex", b"dummy")]);
        assert_eq!(
            resolve_texture_file(&pkg4, "materials/water.json").as_deref(),
            Some("materials/water.tex")
        );
    }

    fn make_test_pkg(entries: &[(&str, &[u8])]) -> Package {
        let mut bytes = Vec::new();
        let version = b"PKGV0001";
        bytes.extend_from_slice(&(version.len() as u32).to_le_bytes());
        bytes.extend_from_slice(version);
        bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());

        let mut offset = 0u32;
        let mut data = Vec::new();
        for (name, content) in entries {
            bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&(content.len() as u32).to_le_bytes());
            offset += content.len() as u32;
            data.extend_from_slice(content);
        }
        bytes.extend_from_slice(&data);
        Package::parse(bytes).unwrap()
    }
}
