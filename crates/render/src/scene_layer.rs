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

use crate::scaling::Scaling;
use crate::scene_transform::{canvas_map, clear_color, view_window, world_transforms, Affine};

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

/// 2D Quad vertices: [posX, posY, texU, texV].
///
/// The quad is modelled as a unit square centred on the object origin: the
/// position spans `-1..1`, so scaling by `size * scale / 2` makes the full
/// quad exactly `size * scale` world units, centred on the origin (this
/// matches how Wallpaper Engine layers are authored). Texture V is inverted so
/// row 0 of the uploaded image (the top) maps to the top of the quad under the
/// y-up orthographic projection.
const QUAD_VERTICES: [f32; 16] = [
    // bottom-left  (position, uv)
    -1.0, -1.0, 0.0, 1.0,
    // bottom-right
    1.0, -1.0, 1.0, 1.0,
    // top-left
    -1.0, 1.0, 0.0, 0.0,
    // top-right
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

    /// A 4x4 for a 2D affine transform; z passes through unchanged.
    pub fn from_affine(m: &Affine) -> Self {
        Mat4([
            m.a, m.b, 0.0, 0.0, //
            m.c, m.d, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            m.e, m.f, 0.0, 1.0, //
        ])
    }
}

/// A rendered 2D image layer.
pub struct RenderLayer {
    pub texture: glow::Texture,
    pub width: f32,
    pub height: f32,
    /// The object's transform composed with every ancestor's (`parent` chains),
    /// in design units. Children are positioned relative to their parent.
    pub world: Affine,
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
    /// Scene space the projection maps to the surface. `None` means "use the
    /// surface size" (the scene does not declare an orthogonal canvas).
    design: Option<(f32, f32)>,
    /// Scene zoom applied on top of the projection (`general.zoom`).
    zoom: f32,
    /// How the design canvas is fitted to the output, mirroring the mode the
    /// user picked for this output (fill / fit / stretch / center).
    scaling: Scaling,
    /// Background painted behind the layers (`general.clearenabled` /
    /// `clearcolor`). Undefined pixels behind a wallpaper are never acceptable.
    clear: [f32; 4],
    is_paused: bool,
}

impl ScenePlayer {
    /// Design canvas for a scene: `general.orthogonalprojection`, defaulting to
    /// the surface dimensions when the scene does not declare one.
    fn design_canvas(scene: &Scene) -> Option<(f32, f32)> {
        let general = scene.general.as_ref()?;
        let ortho = general.orthogonalprojection.as_ref()?;
        match (ortho.width, ortho.height) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w, h)),
            _ => None,
        }
    }
    /// Load a scene from a `scene.pkg` file or directory containing it.
    ///
    /// `scaling` is the output's fitting mode, applied to the design canvas.
    pub fn new(path: &Path, gl: &glow::Context, scaling: Scaling) -> Result<Self> {
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

            let world = world_transforms(&scene.objects);
            let mut layers = Vec::new();

            for (i, obj) in scene.objects.iter().enumerate() {
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
                        world: world[i],
                        color,
                        visible: obj.is_visible(),
                    });
                }
            }

            let design = Self::design_canvas(&scene);
            let zoom = scene
                .general
                .as_ref()
                .and_then(|g| g.zoom)
                .unwrap_or(1.0);

            Ok(ScenePlayer {
                program,
                vao,
                vbo,
                loc_projection,
                loc_model,
                loc_color,
                layers,
                design,
                zoom,
                scaling,
                clear: clear_color(&scene),
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

            // Paint the scene's declared background first. Layers may not cover
            // the whole surface (a letterboxed or centre-scaled canvas, or a
            // scene whose backdrop is smaller than the canvas), and undefined
            // pixels behind a wallpaper are never acceptable.
            gl.clear_color(self.clear[0], self.clear[1], self.clear[2], self.clear[3]);
            gl.clear(glow::COLOR_BUFFER_BIT);

            // Enable standard 2D alpha blending
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);

            gl.use_program(Some(self.program));

            // Map the scene's design canvas onto the surface using this
            // output's scaling mode (fill = cover, fit = letterbox, stretch,
            // centre) — see `scene_transform::canvas_map`. Object origins are
            // canvas-relative: (0, 0) is the canvas' bottom-left corner and +Y
            // points up.
            // Project the design-space window the output surface covers — not
            // the canvas' own extent. When a mode like `Fill` makes the canvas
            // overflow, the window is the visible middle of the canvas and the
            // viewport crops it; projecting the canvas extent instead would
            // leave the overflow margin unpainted. See `scene_transform::view_window`.
            let map = canvas_map(
                self.design,
                self.zoom,
                width as f32,
                height as f32,
                self.scaling,
            );
            let (left, right, bottom, top) = view_window(&map, width as f32, height as f32);
            if std::env::var_os("HYPRWPE_DEBUG_SCENE").is_some() {
                eprintln!(
                    "scene-surface size={}x{} design={:?} zoom={} scaling={:?} map=({:.2},{:.2},{:.4},{:.4}) window=({:.1},{:.1},{:.1},{:.1})",
                    width, height, self.design, self.zoom, self.scaling,
                    map.x, map.y, map.sx, map.sy, left, right, bottom, top
                );
            }
            let proj = Mat4::ortho(left, right, bottom, top, -1000.0, 1000.0);
            if let Some(loc) = &self.loc_projection {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &proj.0);
            }

            gl.bind_vertex_array(Some(self.vao));

            for layer in &self.layers {
                if !layer.visible {
                    continue;
                }

                // The quad spans -1..1 around the object's origin; folding the
                // half-extent into the composed transform makes the full quad
                // exactly `size * scale` design units, centred on the origin and
                // placed through every ancestor's transform.
                let model = Mat4::from_affine(&layer.world.scale_linear(
                    layer.width / 2.0,
                    layer.height / 2.0,
                ));

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

/// Resolve the actual texture bytes for a scene image object.
///
/// Wallpaper Engine scene objects reference their picture through a chain:
///
/// ```text
/// scene.json "image" field
///   -> models/<name>.json      { "material": "materials/<name>.json" }
///   -> materials/<name>.json   { "passes": [{ "textures": ["<bare>"] }] }
///   -> materials/<bare>.tex    (or .png / .jpg next to the material)
/// ```
///
/// Each hop may be skipped when the reference already names a real file
/// (`...png`, `...jpg`, `...tex`). A material-texture name (no extension, the
/// dominant form in the corpus) resolves relative to the package `materials/`
/// directory; see [`resolve_texture_entry`].
fn resolve_texture_file(pkg: &Package, image_ref: &str) -> Option<String> {
    if is_image_file(image_ref) {
        return pkg
            .get(image_ref)
            .is_some()
            .then(|| image_ref.to_string());
    }

    let body = pkg.get_str(image_ref)?;
    let body: serde_json::Value = serde_json::from_str(body).ok()?;

    // A model JSON defers to a material; a material JSON lists textures
    // directly. Both can appear in the scene graph's `image` field.
    if let Some(material) = body.get("material").and_then(|m| m.as_str()) {
        if is_image_file(material) {
            return pkg
                .get(material)
                .is_some()
                .then(|| material.to_string());
        }
        let mat = pkg.get_str(material)?;
        let mat: serde_json::Value = serde_json::from_str(mat).ok()?;
        material_texture_name(pkg, material, &mat)
    } else {
        material_texture_name(pkg, image_ref, &body)
    }
}

/// Given a material JSON body (and its package path), resolve the first
/// texture name it lists to a package entry path.
fn material_texture_name(
    pkg: &Package,
    material_path: &str,
    mat: &serde_json::Value,
) -> Option<String> {
    let texture_name = mat
        .get("passes")
        .and_then(|p| p.as_array())
        .and_then(|p| p.first())
        .and_then(|p0| p0.get("textures"))
        .and_then(|t| t.as_array())
        .and_then(|t| t.first())
        .and_then(|v| v.as_str())
        .or_else(|| {
            mat.get("textures")
                .and_then(|t| t.as_array())
                .and_then(|t| t.first())
                .and_then(|v| v.as_str())
        })?;
    resolve_texture_entry(pkg, material_path, texture_name)
}

fn is_image_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".tex")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".tga")
        || lower.ends_with(".bmp")
}

/// Resolve a material texture name to a real package entry.
///
/// Corpus evidence (75 scene packages, 475 matching references): the names in
/// `passes[].textures` / `textures[]` are **relative to the `materials/`
/// directory**, so `"akalibackground2"` and `"workshop/3518.../背景2"` both
/// resolve to `materials/<name>.tex` (or `.png`/`.jpg`). Handles a name that
/// already carries an image extension too.
fn resolve_texture_entry(pkg: &Package, material_path: &str, name: &str) -> Option<String> {
    let _ = material_path;
    if is_image_file(name) {
        let rooted = format!("materials/{name}");
        let direct = name.to_string();
        return pkg
            .get(&rooted)
            .is_some()
            .then_some(rooted)
            .or_else(|| pkg.get(&direct).is_some().then_some(direct));
    }
    for ext in [".tex", ".png", ".jpg", ".jpeg", ".tga", ".bmp", ""] {
        let cand = format!("materials/{name}{ext}");
        if pkg.get(&cand).is_some() {
            return Some(cand);
        }
    }
    None
}

/// Load the texture bytes for a resolved entry and decode to RGBA.
///
/// `.tex` files go through [`TexImage`] (which understands the `TEXV0005`
/// container and its embedded PNG/JPEG payloads); other image formats decode
/// directly through the `image` crate.
fn load_texture_image(pkg: &Package, path: &str) -> Option<image::RgbaImage> {
    let raw = pkg.get(path)?;
    let img = if path.ends_with(".tex") {
        let tex = TexImage::parse(raw).ok()?;
        tex.to_rgba_image().ok()?
    } else {
        image::load_from_memory(raw).ok()?.to_rgba8()
    };
    // A degenerate decode (1px wide/tall, or wildly mismatched) is almost
    // always a mis-parse of an unsupported .tex sub-format. Uploading it and
    // stretching it across the layer's quad paints coloured scanline noise, so
    // refuse it here and let the layer be skipped.
    let (w, h) = img.dimensions();
    if w <= 1 || h <= 1 || w > 16384 || h > 16384 {
        return None;
    }
    Some(img)
}

/// Load and upload texture data from package to GPU.
unsafe fn load_texture_from_pkg(
    pkg: &Package,
    path: &str,
    gl: &glow::Context,
) -> Option<(glow::Texture, u32, u32)> {
    let img = load_texture_image(pkg, path)?;
    let (width, height) = img.dimensions();
    let pixels = img.into_raw();

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
        // 1. Direct image file present in the package.
        let pkg = make_test_pkg(&[("textures/bg.tex", b"x")]);
        assert_eq!(
            resolve_texture_file(&pkg, "textures/bg.tex").as_deref(),
            Some("textures/bg.tex")
        );

        // 2. Model JSON -> material JSON -> bare name -> materials/<name>.tex.
        //    The texture name is relative to the package `materials/` dir.
        let model = r#"{"material": "materials/forest.json"}"#;
        let mat = r#"{"passes": [{"textures": ["forest"]}]}"#;
        let pkg2 = make_test_pkg(&[
            ("models/forest.json", model.as_bytes()),
            ("materials/forest.json", mat.as_bytes()),
            ("materials/forest.tex", b"tex-data"),
        ]);
        assert_eq!(
            resolve_texture_file(&pkg2, "models/forest.json").as_deref(),
            Some("materials/forest.tex")
        );

        // 3. Material referenced directly by the scene (image == material path).
        let mat3 = r#"{"textures": ["sky/clouds"]}"#;
        let pkg3 = make_test_pkg(&[
            ("materials/sky.json", mat3.as_bytes()),
            ("materials/sky/clouds.tex", b"tex-data"),
        ]);
        assert_eq!(
            resolve_texture_file(&pkg3, "materials/sky.json").as_deref(),
            Some("materials/sky/clouds.tex")
        );

        // 4. Agent fallback: model JSON references a `.json` material that
        //    resolves to a sibling `.tex` only when such a file exists.
        let model4 = r#"{"material": "materials/water.json"}"#;
        let pkg4 = make_test_pkg(&[
            ("models/water.json", model4.as_bytes()),
            ("materials/water.json", br#"{}"#),
            ("materials/water.tex", b"tex-data"),
        ]);
        assert_eq!(
            resolve_texture_file(&pkg4, "models/water.json").as_deref(),
            None // material has no texture names, so nothing resolves
        );

        // 5. Textures key at top level with an extension name kept verbatim.
        let mat5 = r#"{"textures": ["grid.png"]}"#;
        let pkg5 = make_test_pkg(&[
            ("materials/grid.json", mat5.as_bytes()),
            ("materials/grid.png", b"png-data"),
        ]);
        assert_eq!(
            resolve_texture_file(&pkg5, "materials/grid.json").as_deref(),
            Some("materials/grid.png")
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
