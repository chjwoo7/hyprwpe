//! Wallpaper Engine `scene.pkg` scene renderer.
//!
//! Loads `scene.pkg` containers, parses `scene.json` scene graphs, extracts and
//! decompresses textures (.tex / images), and renders 2D transformed layers
//! using OpenGL ES 3.0.

use anyhow::{bail, Context, Result};
use glow::HasContext;
use hyprwpe_core::assets::Resources;
use hyprwpe_core::scene::{ObjectKind, Scene};
use hyprwpe_core::tex::TexImage;
use std::path::Path;

use crate::mesh::MeshRenderer;
use crate::particle::Sim as ParticleSim;
use crate::scaling::Scaling;
use crate::scene_transform::{
    animated_alpha, animated_world_transforms, canvas_map, clear_color, particle_sprite_transform,
    puppet_placement, view_window, world_transforms, Affine,
};
use crate::skin::{deform, Rig};
use hyprwpe_core::animation::Animation;
use hyprwpe_core::mdlv;
use hyprwpe_core::particle as particle_def;
use hyprwpe_core::properties::PropertySet;
use hyprwpe_core::scene::SceneObject;
use std::collections::HashMap;

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
    -1.0, -1.0, 0.0, 1.0, // bottom-right
    1.0, -1.0, 1.0, 1.0, // top-left
    -1.0, 1.0, 0.0, 0.0, // top-right
    1.0, 1.0, 1.0, 0.0,
];

/// Simple 4x4 matrix for 2D orthographic projection and layer transforms.
#[derive(Debug, Clone, Copy)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub fn identity() -> Self {
        Mat4([
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
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

    /// Translation, Z rotation then scale — the local transform a puppet bone
    /// stores, built in this crate's column-vector convention.
    pub fn from_trs(t: [f32; 3], rot: f32, s: [f32; 3]) -> Self {
        let c = rot.cos();
        let sn = rot.sin();
        Mat4([
            s[0] * c,
            s[0] * sn,
            0.0,
            0.0,
            -s[1] * sn,
            s[1] * c,
            0.0,
            0.0,
            0.0,
            0.0,
            s[2],
            0.0,
            t[0],
            t[1],
            t[2],
            1.0,
        ])
    }

    /// Build from the 4x4 the puppet model stores per bone: **row-major with the
    /// translation in the last row** (a row-vector matrix, `v' = v * M`).
    ///
    /// This crate is column-major / column-vector, and the transpose of a
    /// row-vector matrix is exactly what flattening those rows in order gives,
    /// so the elements copy straight across: `(row, col) -> m[col * 4 + row]`.
    pub fn from_row_vector(m: &[[f32; 4]; 4]) -> Self {
        Mat4([
            m[0][0], m[0][1], m[0][2], m[0][3], //
            m[1][0], m[1][1], m[1][2], m[1][3], //
            m[2][0], m[2][1], m[2][2], m[2][3], //
            m[3][0], m[3][1], m[3][2], m[3][3], //
        ])
    }

    /// Inverse of an affine matrix: invert the linear part, rebase the
    /// translation. Perspective is impossible here, so a 3x3 inverse suffices.
    /// A singular matrix (a bone scaled to zero) yields the identity rather than
    /// producing a NaN pose.
    pub fn inverse_affine(&self) -> Mat4 {
        let m = &self.0;
        // Column-major storage: element (row, col) is m[col * 4 + row].
        let a = [[m[0], m[4], m[8]], [m[1], m[5], m[9]], [m[2], m[6], m[10]]];
        let t = [m[12], m[13], m[14]];
        let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        if det.abs() < 1e-12 {
            return Self::identity();
        }
        let d = 1.0 / det;
        // Inverse = adjugate / det, where the adjugate is the transposed
        // cofactor matrix.
        let inv = [
            [
                (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * d,
                (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * d,
                (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * d,
            ],
            [
                (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * d,
                (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * d,
                (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * d,
            ],
            [
                (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * d,
                (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * d,
                (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * d,
            ],
        ];
        // new translation = -(inverse linear part) * t
        let nt = [
            -(inv[0][0] * t[0] + inv[0][1] * t[1] + inv[0][2] * t[2]),
            -(inv[1][0] * t[0] + inv[1][1] * t[1] + inv[1][2] * t[2]),
            -(inv[2][0] * t[0] + inv[2][1] * t[1] + inv[2][2] * t[2]),
        ];
        Mat4([
            inv[0][0], inv[1][0], inv[2][0], 0.0, //
            inv[0][1], inv[1][1], inv[2][1], 0.0, //
            inv[0][2], inv[1][2], inv[2][2], 0.0, //
            nt[0], nt[1], nt[2], 1.0, //
        ])
    }

    /// Transform a 3D point by this matrix.
    pub fn transform_point3(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        [
            m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
            m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
            m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
        ]
    }
}

/// A puppet object: a skinned mesh drawn with its material texture.
///
/// The mesh is uploaded once in its rest pose; each frame the pose for the
/// current time is sampled, the vertices are deformed on the CPU, and only the
/// positions are re-uploaded.
struct PuppetLayer {
    mesh: MeshRenderer,
    texture: glow::Texture,
    rig: Rig,
    model: mdlv::PuppetModel,
    /// Index into the scene's object list, for the animated transform and alpha.
    object_index: usize,
    /// Which of the model's animations to play; the first is the editor default.
    animation: usize,
    /// Model coordinates -> design units (object transform + `cropoffset`).
    placement: Affine,
    color: [f32; 3],
    visible: bool,
    /// Reused deformation buffer, so a frame allocates nothing.
    positions: Vec<[f32; 2]>,
}

/// A running particle system: one emitter group with its sprite texture.
///
/// A particle object may name several definitions (its own plus `children`), so
/// an object holds a list of these, all sharing the object's placement.
struct ParticleLayer {
    sim: ParticleSim,
    texture: glow::Texture,
    /// Object transform applied to every sprite this system emits.
    placement: Affine,
    color: [f32; 3],
    object_index: usize,
    visible: bool,
    /// Sprites for the current frame, reused so a frame allocates nothing once
    /// the population has settled.
    sprites: Vec<crate::particle::Sprite>,
    /// The scene-clock time this simulation has been advanced to.
    ///
    /// Stepping on `Instant::now()` deltas made a fixed-time render spawn
    /// nothing: one frame advances the sim by ~0s. Tracking the scene clock
    /// instead means the same code is right for a live daemon (the clock is
    /// monotonic) and for a headless render at an arbitrary time.
    sim_time: f32,
}

/// A rendered 2D image layer.
pub struct RenderLayer {
    pub texture: glow::Texture,
    pub width: f32,
    pub height: f32,
    /// The object's effect chains, in the order the engine applies them.
    ///
    /// One per effect file, because an object may stack several and each reads
    /// the previous one's result. Built once at load: a chain owns a linked
    /// program per pass and a pair of render targets, so rebuilding it per frame
    /// would be both slow and wrong.
    pub effects: Vec<crate::effect_pass::Chain>,
    /// The object's transform composed with every ancestor's (`parent` chains),
    /// in design units. Children are positioned relative to their parent. Used
    /// only when the scene has no animation; otherwise it is recomputed per
    /// frame so animated properties (and animated ancestors) take effect.
    pub world: Affine,
    /// Index of the source object, so an animated property can be looked up.
    pub object_index: usize,
    /// Base colour; the alpha component comes from the object (or its
    /// animation) each frame.
    pub color: [f32; 3],
    pub visible: bool,
    /// The material samples a render target (`_rt_...`), so this layer's texture
    /// is the frame drawn so far rather than a file. The copy is taken each time
    /// the layer is drawn, which is what makes it a post-process.
    pub samples_frame: bool,
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
    /// Puppet objects (skinned meshes) in scene order after the quads.
    puppets: Vec<PuppetLayer>,
    /// Particle systems, drawn after the image layers and puppets.
    particles: Vec<ParticleLayer>,
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
    /// The scene graph and its property animations, kept so animated transforms
    /// can be evaluated per frame.
    objects: Vec<SceneObject>,
    animations: Vec<HashMap<String, Animation>>,
    /// Whether any object animates; when false the loop skips recomputation.
    animated: bool,
    /// Clock origin for animation time.
    start: std::time::Instant,
    /// When the last frame was actually drawn. A frame cap needs to know how
    /// long it has been, and the alternative - trusting the compositor's frame
    /// callbacks - gives up the cap entirely (they arrive at the panel's refresh
    /// rate, not ours).
    last_draw: std::time::Instant,
    /// Whether effect chains run. Off makes the two paths directly comparable.
    effects_enabled: bool,
    /// How many objects carry an effect chain, for `describe`.
    effect_count: usize,
    /// An off-screen target at the output's size, reused by every effect pass.
    /// One is enough because layers are drawn one at a time.
    object_target: Option<(glow::Framebuffer, glow::Texture, i32, i32)>,
    /// A copy of the frame drawn so far, for layers whose material samples a
    /// render target. Sized to the output and refilled whenever that changes.
    frame_copy: Option<(glow::Texture, i32, i32)>,
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
        Self::with_properties(path, gl, scaling, &PropertySet::default())
    }

    /// Load a scene with a wallpaper's user properties applied.
    ///
    /// Bindings are resolved on the raw document before it is parsed, so nothing
    /// downstream needs to know the binding syntax exists: layers, puppets,
    /// particles and animations all read plain values.
    pub fn with_properties(
        path: &Path,
        gl: &glow::Context,
        scaling: Scaling,
        properties: &PropertySet,
    ) -> Result<Self> {
        let pkg_path = if path.is_dir() {
            path.join("scene.pkg")
        } else {
            path.to_path_buf()
        };

        let pkg = Resources::open(&pkg_path)
            .with_context(|| format!("opening scene package {}", pkg_path.display()))?;

        let scene_json = pkg
            .get_str("scene.json")
            .context("scene.pkg contains no scene.json")?;
        let scene = Scene::from_json_with_properties(&scene_json, properties)
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
            let mut effect_count = 0usize;
            let mut puppets = Vec::new();
            let mut particles = Vec::new();

            for (i, obj) in scene.objects.iter().enumerate() {
                // Particle systems are their own object kind; they have no quad.
                if let Some(pref) = obj.particle_path() {
                    let color_rgb = obj
                        .color
                        .map(|c| [c.x(), c.y(), c.z()])
                        .unwrap_or([1.0, 1.0, 1.0]);
                    particles.extend(load_particle_layers(
                        &pkg,
                        gl,
                        &pref,
                        &world[i],
                        color_rgb,
                        i,
                        obj.is_visible(),
                    ));
                    continue;
                }

                if obj.kind() != ObjectKind::Image {
                    continue;
                }
                let Some(mat_path) = obj.image_path() else {
                    continue;
                };

                // A model naming a `puppet` is a deforming mesh: parse it, build
                // the rig, and draw triangles instead of a textured quad.
                if let Some(mdl_path) = resolve_puppet_file(&pkg, &mat_path) {
                    if let (Some(raw), Some((tex_handle, _, _))) = (
                        pkg.get(&mdl_path),
                        resolve_texture_file(&pkg, &mat_path)
                            .and_then(|name| load_texture_from_pkg(&pkg, &name, gl)),
                    ) {
                        if let Ok(model) = mdlv::parse(&raw) {
                            let mut mesh = match MeshRenderer::new(gl) {
                                Ok(m) => m,
                                Err(_) => continue,
                            };
                            let rest: Vec<[f32; 2]> =
                                model.mesh.positions.iter().map(|p| [p[0], p[1]]).collect();
                            if mesh
                                .upload(gl, &rest, &model.mesh.uvs, &model.mesh.indices)
                                .is_ok()
                            {
                                let color_rgb = obj
                                    .color
                                    .map(|c| [c.x(), c.y(), c.z()])
                                    .unwrap_or([1.0, 1.0, 1.0]);
                                let crop = model_cropoffset(&pkg, &mat_path);
                                puppets.push(PuppetLayer {
                                    mesh,
                                    texture: tex_handle,
                                    rig: Rig::new(&model),
                                    model,
                                    object_index: i,
                                    animation: 0,
                                    placement: puppet_placement(&world[i], crop),
                                    color: color_rgb,
                                    visible: obj.is_visible(),
                                    positions: Vec::new(),
                                });
                            }
                        }
                    }
                    continue;
                }

                // Classify before loading: a material that names no texture is a
                // flat fill of the object's colour (the engine's `solidlayer`),
                // and one that names a render target is the frame drawn so far.
                // Only a name that is actually missing is worth dropping - and
                // that is silent by default, because most packages reference
                // engine assets that exist only on a machine with Wallpaper
                // Engine installed.
                let (tex_handle, tex_name, tex_w, tex_h, samples_frame) =
                    match material_texture(&pkg, &mat_path) {
                        MaterialTexture::File(name) => match load_texture_from_pkg(&pkg, &name, gl)
                        {
                            Some((handle, w, h)) => (handle, name, w, h, false),
                            None => {
                                if std::env::var_os("HYPRWPE_DEBUG_LAYERS").is_some() {
                                    eprintln!(
                                        "  layer obj {i} {:?}: {name:?} would not decode",
                                        obj.name.as_deref().unwrap_or("?")
                                    );
                                }
                                continue;
                            }
                        },
                        MaterialTexture::Unbound => match solid_texture(gl) {
                            Some(handle) => (handle, String::from("<flat fill>"), 1, 1, false),
                            None => continue,
                        },
                        // A render-target layer samples the frame drawn so far.
                        // The real texture is a copy taken each time it is drawn;
                        // the 1x1 here is only so the layer owns something to
                        // free, like every other layer.
                        MaterialTexture::RenderTarget(name) => match solid_texture(gl) {
                            Some(handle) => (handle, name, 1, 1, true),
                            None => continue,
                        },
                        MaterialTexture::Missing => {
                            if std::env::var_os("HYPRWPE_DEBUG_LAYERS").is_some() {
                                eprintln!(
                                    "  layer obj {i} {:?}: unresolved from {:?}",
                                    obj.name.as_deref().unwrap_or("?"),
                                    mat_path
                                );
                            }
                            continue;
                        }
                    };

                {
                    let mut size = [tex_w as f32, tex_h as f32];
                    if let Some(s) = obj.size() {
                        if s[0] > 0.0 && s[1] > 0.0 {
                            size = s;
                        }
                    }

                    let color_rgb = obj
                        .color
                        .map(|c| [c.x(), c.y(), c.z()])
                        .unwrap_or([1.0, 1.0, 1.0]);

                    // An object's effects are a chain of shader passes over its
                    // own render, so the chain is built here, once, where the
                    // package and the object's values are both in hand.
                    let effects = build_effect_chains(&pkg, gl, obj);
                    effect_count += effects.iter().map(|c| c.len()).sum::<usize>();

                    if std::env::var_os("HYPRWPE_DEBUG_LAYERS").is_some() {
                        eprintln!(
                            "  draw obj {i} {:?}: {}x{} color {:?} visible {} tex {tex_name}",
                            obj.name.as_deref().unwrap_or("?"),
                            size[0],
                            size[1],
                            color_rgb,
                            obj.is_visible()
                        );
                    }

                    layers.push(RenderLayer {
                        texture: tex_handle,
                        width: size[0],
                        height: size[1],
                        effects,
                        samples_frame,
                        world: world[i],
                        object_index: i,
                        color: color_rgb,
                        visible: obj.is_visible(),
                    });
                }
            }

            let design = Self::design_canvas(&scene);
            let zoom = scene.general.as_ref().and_then(|g| g.zoom).unwrap_or(1.0);
            let animated = scene.animations.iter().any(|m| !m.is_empty());

            Ok(ScenePlayer {
                program,
                vao,
                vbo,
                loc_projection,
                loc_model,
                loc_color,
                layers,
                puppets,
                particles,
                design,
                zoom,
                scaling,
                clear: clear_color(&scene),
                objects: scene.objects.clone(),
                animations: scene.animations.clone(),
                animated,
                start: std::time::Instant::now(),
                last_draw: std::time::Instant::now(),
                effects_enabled: true,
                effect_count,
                object_target: None,
                frame_copy: None,
                is_paused: false,
            })
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.is_paused = paused;
    }

    /// Move the scene's clock to `t` seconds, so a render taken next reflects a
    /// moment that is not the very start.
    ///
    /// Particle systems spawn on a schedule and animations begin at their first
    /// keyframe, so a frame at t=0 understates a scene: measuring a library at
    /// zero would call a working wallpaper blank.
    pub fn advance_to(&mut self, t: f32) {
        let t = t.max(0.0);
        self.start = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs_f32(t))
            .unwrap_or_else(std::time::Instant::now);
        // Bring the simulations to that moment in the simulator's own fixed
        // steps, so a render at an arbitrary time shows a settled population
        // rather than one that has existed for a single frame.
        for p in &mut self.particles {
            p.sprites = p.sim.advance_to(t);
            p.sim_time = t;
        }
    }

    /// Whether a frame is due, given an optional cap.
    ///
    /// `None` means "draw whenever the compositor asks", which is the default
    /// and matches the previous behaviour. A cap lets a 165 Hz panel drive an
    /// animated wallpaper at 60: the frame callback still arrives 165 times a
    /// second, but the geometry, the skinning and the particle step are skipped
    /// for the ones that fall inside the interval.
    pub fn due(&self, interval: Option<std::time::Duration>) -> bool {
        match interval {
            None => true,
            Some(i) => self.last_draw.elapsed() >= i,
        }
    }

    /// Record that a frame was just drawn.
    pub fn mark_drawn(&mut self) {
        self.last_draw = std::time::Instant::now();
    }

    /// Turn effect chains on or off. With them off the renderer draws exactly
    /// what it drew before they existed, which is what makes the two paths
    /// comparable in a measurement.
    pub fn set_effects_enabled(&mut self, enabled: bool) {
        self.effects_enabled = enabled;
    }

    /// A short description of what this scene contains, for tools and logs.
    ///
    /// `resolved / image objects` is the honest ratio: the denominator counts the
    /// image-kind objects the scene actually wants drawn (objects hidden by a
    /// property gate are not counted against us), and the numerator counts the
    /// ones that became something drawable - a textured quad *or* a skinned mesh,
    /// since a model naming a `puppet` is drawn as triangles rather than a quad.
    pub fn describe(&self) -> String {
        let image_objects = self
            .objects
            .iter()
            .filter(|o| {
                o.kind() == ObjectKind::Image && o.particle_path().is_none() && o.is_visible()
            })
            .count();
        let quads = self.layers.iter().filter(|l| l.visible).count();
        let puppets = self.puppets.iter().filter(|p| p.visible).count();
        format!(
            "{} resolved of {} image objects ({} quads, {} puppets; {} with effects, {} effect passes), {} particle systems, design {:?}, zoom {}, scaling {:?}",
            quads + puppets,
            image_objects,
            quads,
            puppets,
            self.layers.iter().filter(|l| !l.effects.is_empty()).count(),
            self.effect_count,
            self.particles.len(),
            self.design,
            self.zoom,
            self.scaling,
        )
    }

    /// The colour painted behind the layers, as `describe`'s companions need it
    /// to tell covered pixels from bare ones.
    pub fn clear_color(&self) -> [f32; 4] {
        self.clear
    }

    /// A texture holding the frame drawn so far, for a render-target layer.
    ///
    /// Resized on demand: the output can change (a monitor switch, a scaling
    /// change) and a copy of the wrong size would break the layer's one-to-one
    /// mapping with the screen.
    fn frame_copy_texture(
        &mut self,
        gl: &glow::Context,
        width: i32,
        height: i32,
    ) -> Option<glow::Texture> {
        if let Some((texture, w, h)) = self.frame_copy {
            if w == width && h == height {
                return Some(texture);
            }
            unsafe {
                gl.delete_texture(texture);
            }
            self.frame_copy = None;
        }

        unsafe {
            let texture = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            // Storage only; `copy_tex_image_2d` fills it each time a layer uses it.
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            for (param, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, param, value);
            }
            self.frame_copy = Some((texture, width, height));
            Some(texture)
        }
    }

    /// The off-screen target effect passes render into, created on demand and
    /// recreated when the output changes size.
    ///
    /// One target is enough for the whole scene because layers are drawn one at a
    /// time, and it is reused across frames so a steady state allocates nothing.
    fn object_target(
        &mut self,
        gl: &glow::Context,
        width: i32,
        height: i32,
    ) -> Option<(glow::Framebuffer, glow::Texture)> {
        if let Some((fbo, texture, w, h)) = self.object_target {
            if w == width && h == height {
                return Some((fbo, texture));
            }
            unsafe {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(texture);
            }
            self.object_target = None;
        }
        unsafe {
            let texture = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            let fbo = gl.create_framebuffer().ok()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.object_target = Some((fbo, texture, width, height));
            Some((fbo, texture))
        }
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

            // Animated properties are evaluated on the render clock; a scene
            // with no animation reuses the transform computed at load.
            let t = self.start.elapsed().as_secs_f32();
            let animated_world = if self.animated {
                animated_world_transforms(&self.objects, &self.animations, t)
            } else {
                Vec::new()
            };

            // `apply` needs each chain mutably while the loop also reads
            // `self.objects`/`self.animations`, which the borrow checker will not
            // allow through an indexed field. Taking the vector out and putting it
            // back is a pointer swap, and nothing below can return early.
            let mut layers = std::mem::take(&mut self.layers);
            // Whatever framebuffer the caller had bound (the compositor's surface
            // is `None`/0) has to be restored after an effect pass renders
            // off-screen.
            let bound_fbo = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32;

            for layer in layers.iter_mut() {
                if !layer.visible {
                    continue;
                }

                // Re-established per layer, because an effect layer changes the
                // projection for its own fullscreen composite.
                if let Some(loc) = &self.loc_projection {
                    gl.uniform_matrix_4_f32_slice(Some(loc), false, &proj.0);
                }

                let world = if self.animated {
                    animated_world[layer.object_index]
                } else {
                    layer.world
                };

                // The quad spans -1..1 around the object's origin; folding the
                // half-extent into the composed transform makes the full quad
                // exactly `size * scale` design units, centred on the origin and
                // placed through every ancestor's transform.
                let model =
                    Mat4::from_affine(&world.scale_linear(layer.width / 2.0, layer.height / 2.0));

                if let Some(loc) = &self.loc_model {
                    gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.0);
                }
                if let Some(loc) = &self.loc_color {
                    let alpha = if self.animated {
                        animated_alpha(
                            &self.objects[layer.object_index],
                            &self.animations[layer.object_index],
                            t,
                        )
                    } else {
                        self.objects[layer.object_index].alpha()
                    };
                    gl.uniform_4_f32(
                        Some(loc),
                        layer.color[0],
                        layer.color[1],
                        layer.color[2],
                        alpha,
                    );
                }

                let alpha = if self.animated {
                    animated_alpha(
                        &self.objects[layer.object_index],
                        &self.animations[layer.object_index],
                        t,
                    )
                } else {
                    self.objects[layer.object_index].alpha()
                };

                // A layer whose material samples a render target (the engine's
                // full-screen layer materials) uses the frame drawn so far. The
                // copy is taken here, not at load, because that is exactly what
                // makes the layer a post-process over everything before it.
                let mut texture = layer.texture;
                if layer.samples_frame {
                    match self.frame_copy_texture(gl, width, height) {
                        Some(copy) => {
                            gl.bind_texture(glow::TEXTURE_2D, Some(copy));
                            gl.copy_tex_image_2d(
                                glow::TEXTURE_2D,
                                0,
                                glow::RGBA,
                                0,
                                0,
                                width,
                                height,
                                0,
                            );
                            texture = copy;
                        }
                        // No copy available: drawing the 1x1 placeholder would
                        // paint a white quad over the scene, so skip instead.
                        None => continue,
                    }
                }

                let debug_fx = std::env::var_os("HYPRWPE_DEBUG_FX").is_some();
                if debug_fx {
                    eprintln!(
                        "fx: layer obj={} chain={:?} enabled={} bound_fbo={}",
                        layer.object_index,
                        layer.effects.len(),
                        self.effects_enabled,
                        bound_fbo
                    );
                }
                if layer.effects.is_empty() || !self.effects_enabled {
                    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    continue;
                }

                // Effect path: draw the object off-screen, run its chain, then
                // composite the result. It is rendered at the output's size with
                // the same projection, so a pass that reads screen coordinates
                // sees what the engine would have given it.
                let Some((object_fbo, object_texture)) = self.object_target(gl, width, height)
                else {
                    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    continue;
                };
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(object_fbo));
                gl.viewport(0, 0, width, height);
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                if let Some(loc) = &self.loc_model {
                    gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.0);
                }
                if let Some(loc) = &self.loc_color {
                    gl.uniform_4_f32(
                        Some(loc),
                        layer.color[0],
                        layer.color[1],
                        layer.color[2],
                        alpha,
                    );
                }
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

                if debug_fx {
                    // What the object rendered as, before any effect touches it.
                    // A chain cannot be blamed for a blank input.
                    let mut px = [0u8; 4];
                    gl.read_pixels(
                        width / 2,
                        height / 2,
                        1,
                        1,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(&mut px)),
                    );
                    eprintln!(
                        "fx: object render center = ({}, {}, {}, {}) alpha={}",
                        px[0], px[1], px[2], px[3], alpha
                    );
                }

                // Each chain reads what the previous one produced, so the effect
                // stack composes exactly as the engine's does.
                let extra: Vec<Option<glow::Texture>> = vec![None; 8];
                let mut produced: Result<glow::Texture> = Ok(object_texture);
                for chain in layer.effects.iter_mut() {
                    produced =
                        produced.and_then(|src| chain.apply(gl, src, (width, height), &extra, t));
                    if produced.is_err() {
                        break;
                    }
                }
                let result = produced;

                // Restore everything the chain changed. A pass owns no state
                // outside its own targets: it binds its own VAO and leaves none
                // bound, sets its own blending, and unbinds the program - so the
                // scene's drawing (this layer's composite, then puppets and
                // particles) gets nothing drawn at all unless each is put back.
                gl.bind_framebuffer(glow::FRAMEBUFFER, restore(bound_fbo));
                gl.viewport(0, 0, width, height);
                gl.use_program(Some(self.program));
                gl.bind_vertex_array(Some(self.vao));
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                gl.disable(glow::DEPTH_TEST);
                if let Some(loc) = &self.loc_projection {
                    gl.uniform_matrix_4_f32_slice(Some(loc), false, &proj.0);
                }
                if let Some(loc) = &self.loc_model {
                    gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.0);
                }
                if let Some(loc) = &self.loc_color {
                    gl.uniform_4_f32(
                        Some(loc),
                        layer.color[0],
                        layer.color[1],
                        layer.color[2],
                        alpha,
                    );
                }

                if debug_fx {
                    eprintln!(
                        "fx: apply -> {:?}",
                        result.as_ref().map(|_| "ok").map_err(|e| format!("{e:#}"))
                    );
                }
                match result {
                    Ok(passed) => {
                        // The chain's output is already in output space, so it
                        // composites as a fullscreen quad: clip-space projection,
                        // no model transform, no tint.
                        if let Some(loc) = &self.loc_projection {
                            let full = Mat4::ortho(-1.0, 1.0, -1.0, 1.0, -1.0, 1.0);
                            gl.uniform_matrix_4_f32_slice(Some(loc), false, &full.0);
                        }
                        if let Some(loc) = &self.loc_model {
                            gl.uniform_matrix_4_f32_slice(Some(loc), false, &Mat4::identity().0);
                        }
                        if let Some(loc) = &self.loc_color {
                            gl.uniform_4_f32(Some(loc), 1.0, 1.0, 1.0, 1.0);
                        }
                        gl.bind_texture(glow::TEXTURE_2D, Some(passed));
                        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    }
                    Err(e) => {
                        // A chain that fails mid-frame must not leave a hole:
                        // draw the object plainly instead.
                        eprintln!(
                            "hyprwpe: effect chain for object {}: {e:#}",
                            layer.object_index
                        );
                        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    }
                }
            }
            self.layers = layers;

            // Puppets (skinned meshes). The pose is sampled for this instant, the
            // vertices are deformed on the CPU, and only the positions are
            // re-uploaded before drawing — the rest of the vertex data is static.
            for p in &mut self.puppets {
                if !p.visible {
                    continue;
                }
                let anim = p.model.animations.get(p.animation);
                let pose = p.rig.pose(anim, t);
                deform(&p.model.mesh, &pose, &mut p.positions);
                if p.positions.len() != p.model.mesh.positions.len() {
                    continue;
                }
                p.mesh.update_positions(gl, &p.positions);
                let alpha = if self.animated {
                    animated_alpha(
                        &self.objects[p.object_index],
                        &self.animations[p.object_index],
                        t,
                    )
                } else {
                    self.objects[p.object_index].alpha()
                };
                let model = Mat4::from_affine(&p.placement);
                p.mesh.draw(
                    gl,
                    &proj,
                    &model,
                    p.texture,
                    [p.color[0], p.color[1], p.color[2], alpha],
                );
            }

            // Particle systems: advance each simulation by the wall-clock gap
            // since its last step and draw every sprite as a quad.
            if !self.particles.is_empty() {
                gl.bind_vertex_array(Some(self.vao));
                for p in &mut self.particles {
                    if !p.visible {
                        continue;
                    }
                    // Advance on the scene clock, not the wall clock. Stepping on
                    // `Instant` deltas meant a headless render at a fixed time
                    // advanced the sim by ~0s and spawned nothing, however long
                    // the scene had "been running".
                    //
                    // Written as a guarded subtraction rather than `clamp`,
                    // which panics on a NaN and would take the renderer down.
                    let dt = if t > p.sim_time {
                        (t - p.sim_time).min(0.1)
                    } else {
                        0.0
                    };
                    p.sim_time = t;
                    p.sprites = p.sim.step(dt);
                    let placement = if self.animated {
                        animated_world[p.object_index]
                    } else {
                        p.placement
                    };
                    let alpha = if self.animated {
                        animated_alpha(
                            &self.objects[p.object_index],
                            &self.animations[p.object_index],
                            t,
                        )
                    } else {
                        self.objects[p.object_index].alpha()
                    };

                    gl.bind_texture(glow::TEXTURE_2D, Some(p.texture));
                    for s in &p.sprites {
                        let model = Mat4::from_affine(&particle_sprite_transform(
                            &placement, s.pos, s.rotation, s.size,
                        ));
                        if let Some(loc) = &self.loc_model {
                            gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.0);
                        }
                        if let Some(loc) = &self.loc_color {
                            gl.uniform_4_f32(
                                Some(loc),
                                s.color[0] * p.color[0],
                                s.color[1] * p.color[1],
                                s.color[2] * p.color[2],
                                s.alpha * alpha,
                            );
                        }
                        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    }
                }
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
                for chain in &layer.effects {
                    chain.destroy(gl);
                }
                gl.delete_texture(layer.texture);
            }
            if let Some((fbo, texture, _, _)) = self.object_target {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(texture);
            }
            if let Some((texture, _, _)) = self.frame_copy {
                gl.delete_texture(texture);
            }
            for p in self.puppets {
                gl.delete_texture(p.texture);
                p.mesh.destroy(gl);
            }
            for p in self.particles {
                gl.delete_texture(p.texture);
            }
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
        }
    }
}

/// The framebuffer a `DRAW_FRAMEBUFFER_BINDING` id refers to.
///
/// GL reports the bound framebuffer as an integer, and 0 means "the default
/// one" - which `glow` spells `None`. Reconstructing it is how the renderer
/// restores what the caller had bound after an off-screen pass.
fn restore(id: u32) -> Option<glow::NativeFramebuffer> {
    std::num::NonZeroU32::new(id).map(glow::NativeFramebuffer)
}

/// Build the effect chains an object names, in order.
///
/// One chain per effect file: an object may stack several effects, and the engine
/// applies them in order, each reading the previous result. The values a chain
/// resolves are the object's own per-pass overrides.
fn build_effect_chains(
    res: &Resources,
    gl: &glow::Context,
    obj: &SceneObject,
) -> Vec<crate::effect_pass::Chain> {
    let mut chains = Vec::new();
    for effect in &obj.effects {
        if !effect.visible.unwrap_or(true) {
            continue;
        }
        let Some(file) = effect.file.as_ref() else {
            continue;
        };
        let Some(body) = res.get_str(file) else {
            eprintln!("hyprwpe: effect {file} is not in the package");
            continue;
        };
        let values: Vec<Option<serde_json::Map<String, serde_json::Value>>> = effect
            .passes
            .iter()
            .map(|p| p.constantshadervalues.clone())
            .collect();
        // The combos a scene selects are compile-time: they decide which branch
        // of the shader's `#if` chain is built, so a pass with the wrong branch
        // does not merely look different, it can render nothing at all.
        let combos: Vec<Vec<String>> = effect.passes.iter().map(|p| p.defines()).collect();
        match crate::effect_pass::Chain::with_combos(gl, res, &body, &values, &combos) {
            Ok(chain) if !chain.is_empty() => chains.push(chain),
            Ok(_) => {}
            Err(e) => eprintln!("hyprwpe: effect {file}: {e:#}"),
        }
    }
    chains
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
fn resolve_texture_file(pkg: &Resources, image_ref: &str) -> Option<String> {
    match material_texture(pkg, image_ref) {
        MaterialTexture::File(name) => Some(name),
        _ => None,
    }
}

/// What a scene object's `image` reference finally names.
///
/// The distinction matters because three very different things hide behind the
/// same field, and only one of them is "skip this object":
///
/// * a texture file, which is drawn as a textured quad;
/// * a **render target** (`_rt_...`), which is the frame drawn so far rather
///   than any file - the engine's own full-screen layer materials are built on
///   it;
/// * **no texture at all**, which is a flat fill of the object's colour (the
///   engine's `solidlayer` material is exactly this);
/// * a name that is simply not here, which is the only case worth dropping.
enum MaterialTexture {
    File(String),
    RenderTarget(String),
    Unbound,
    Missing,
}

/// Walk the texture chain and classify what it names; see [`MaterialTexture`]
/// and [`resolve_texture_file`] for the chain itself.
fn material_texture(pkg: &Resources, image_ref: &str) -> MaterialTexture {
    if is_image_file(image_ref) {
        return if pkg.get(image_ref).is_some() {
            MaterialTexture::File(image_ref.to_string())
        } else {
            MaterialTexture::Missing
        };
    }

    let Some(body) = pkg.get_str(image_ref) else {
        return MaterialTexture::Missing;
    };
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&body) else {
        return MaterialTexture::Missing;
    };

    // A model JSON defers to a material; a material JSON lists textures
    // directly. Both can appear in the scene graph's `image` field.
    let (material_path, mat) = match body.get("material").and_then(|m| m.as_str()) {
        Some(material) if is_image_file(material) => {
            return if pkg.get(material).is_some() {
                MaterialTexture::File(material.to_string())
            } else {
                MaterialTexture::Missing
            };
        }
        Some(material) => {
            let Some(text) = pkg.get_str(material) else {
                return MaterialTexture::Missing;
            };
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
                return MaterialTexture::Missing;
            };
            (material.to_string(), parsed)
        }
        None => (image_ref.to_string(), body),
    };

    let Some(name) = declared_texture_name(&mat) else {
        // The material declares no texture: a flat fill, not a failure.
        return MaterialTexture::Unbound;
    };
    if is_render_target(&name) {
        return MaterialTexture::RenderTarget(name);
    }
    match resolve_texture_entry(pkg, &material_path, &name) {
        Some(path) => MaterialTexture::File(path),
        None => MaterialTexture::Missing,
    }
}

/// The first texture name a material declares, if any.
fn declared_texture_name(mat: &serde_json::Value) -> Option<String> {
    mat.get("passes")
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
        })
        .map(str::to_string)
}

/// A name like `_rt_FullFrameBuffer` is a render target the engine fills in
/// itself, not a file to look up.
fn is_render_target(name: &str) -> bool {
    name.starts_with("_rt_")
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

/// Load the particle systems an object names: its own definition plus every
/// `children` definition, recursively.
///
/// Each definition becomes an independent simulation, all sharing the object's
/// placement. A definition without a material has no sprite to draw and is
/// skipped rather than rendered as nothing.
///
/// # Safety
/// Calls GL through `load_texture_from_pkg`, so a GL context must be current.
unsafe fn load_particle_layers(
    pkg: &Resources,
    gl: &glow::Context,
    root_ref: &str,
    placement: &Affine,
    color: [f32; 3],
    object_index: usize,
    visible: bool,
) -> Vec<ParticleLayer> {
    let mut out = Vec::new();
    let mut queue = vec![root_ref.to_string()];
    let mut seen = std::collections::HashSet::new();
    // A stable per-object seed keeps a scene's particle layout repeatable.
    let mut seed = 0x9e37_79b9u32 ^ (object_index as u32 + 1).wrapping_mul(2654435761);

    while let Some(path) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(body) = pkg.get_str(&path) else {
            continue;
        };
        let Ok(system) = particle_def::parse(&body) else {
            continue;
        };
        for child in &system.children {
            queue.push(child.clone());
        }
        let Some(mat_path) = system.material.clone() else {
            continue;
        };
        let Some(mat_body) = pkg.get_str(&mat_path) else {
            continue;
        };
        let Some(tex_name) = particle_def::material_texture(&mat_body) else {
            continue;
        };
        let Some(entry) = resolve_texture_entry(pkg, &mat_path, &tex_name) else {
            continue;
        };
        let Some((texture, _, _)) = load_texture_from_pkg(pkg, &entry, gl) else {
            continue;
        };
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let control_points = system.control_points.clone();
        out.push(ParticleLayer {
            sim: ParticleSim::new(system, control_points, seed),
            texture,
            placement: *placement,
            color,
            object_index,
            visible,
            sprites: Vec::new(),
            sim_time: 0.0,
        });
    }
    out
}

/// The `cropoffset` a model JSON may carry, in model space.
///
/// It shifts the mesh before the object transform, and is the only model-space
/// adjustment a puppet needs.
fn model_cropoffset(pkg: &Resources, image_ref: &str) -> [f32; 2] {
    let Some(body) = pkg.get_str(image_ref) else {
        return [0.0, 0.0];
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return [0.0, 0.0];
    };
    let Some(s) = v.get("cropoffset").and_then(|c| c.as_str()) else {
        return [0.0, 0.0];
    };
    let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
    let x = it.next().unwrap_or(0.0);
    let y = it.next().unwrap_or(0.0);
    [x, y]
}

/// If the object's image resolves through a model JSON that names a puppet
/// model, return the package path of that `.mdl`.
///
/// A puppet replaces the flat quad with a deforming mesh, so the renderer needs
/// to know about it rather than drawing the material texture on a rectangle.
pub fn resolve_puppet_file(pkg: &Resources, image_ref: &str) -> Option<String> {
    if is_image_file(image_ref) {
        return None;
    }
    let body = pkg.get_str(image_ref)?;
    let body: serde_json::Value = serde_json::from_str(&body).ok()?;
    let puppet = body.get("puppet")?.as_str()?;
    pkg.get(puppet).is_some().then(|| puppet.to_string())
}

/// Resolve a material texture name to a real package entry.
///
/// Corpus evidence (75 scene packages, 475 matching references): the names in
/// `passes[].textures` / `textures[]` are **relative to the `materials/`
/// directory**, so `"akalibackground2"` and `"workshop/3518.../背景2"` both
/// resolve to `materials/<name>.tex` (or `.png`/`.jpg`). Handles a name that
/// already carries an image extension too.
fn resolve_texture_entry(pkg: &Resources, material_path: &str, name: &str) -> Option<String> {
    let _ = material_path;
    // A texture name is written three ways in the corpus: as a file relative to
    // the package (`sky/clouds.png`), as a bare name the entry supplies the
    // extension for (`cat_rb` -> `materials/cat_rb.tex`), and as a name that
    // *looks* like an image but is only a base - a creator's own asset embedded
    // behind it (`ojedehfdz.jpeg` -> `materials/ojedehfdz.jpeg.tex`).
    //
    // Branching on whether the name looks like an image handled the first and
    // missed the third, because a name that already ends in `.jpeg` never got
    // the extension tried. Trying the name as written, then with each extension,
    // covers all three without having to guess which kind it is.
    let mut candidates: Vec<String> = Vec::new();
    for root in ["materials/", ""] {
        candidates.push(format!("{root}{name}"));
        for ext in [".tex", ".png", ".jpg", ".jpeg", ".tga", ".bmp"] {
            candidates.push(format!("{root}{name}{ext}"));
        }
    }
    candidates.into_iter().find(|c| pkg.get(c).is_some())
}

/// Load the texture bytes for a resolved entry and decode to RGBA.
///
/// `.tex` files go through [`TexImage`] (which understands the `TEXV0005`
/// container and its embedded PNG/JPEG payloads); other image formats decode
/// directly through the `image` crate.
fn load_texture_image(pkg: &Resources, path: &str) -> Option<image::RgbaImage> {
    let (raw, sidecar) = pkg.texture(path)?;
    let img = if path.ends_with(".tex") {
        let tex = TexImage::parse_with_sidecar(&raw, sidecar.as_deref()).ok()?;
        tex.to_rgba_image().ok()?
    } else {
        image::load_from_memory(&raw).ok()?.to_rgba8()
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

/// A 1x1 opaque white texture, for a material that declares no texture at all.
///
/// The engine's `solidlayer` material is exactly this - a `flat` shader with no
/// sampler - so the object's own colour is the whole picture. One texture per
/// such layer keeps ownership simple: it is four bytes, and every layer is freed
/// the same way regardless of where its texture came from.
unsafe fn solid_texture(gl: &glow::Context) -> Option<glow::Texture> {
    let texture = gl.create_texture().ok()?;
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA as i32,
        1,
        1,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(&[255u8, 255, 255, 255])),
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::NEAREST as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::NEAREST as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_S,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_T,
        glow::CLAMP_TO_EDGE as i32,
    );
    Some(texture)
}

/// Load and upload texture data from package to GPU.
unsafe fn load_texture_from_pkg(
    pkg: &Resources,
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

    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_S,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_T,
        glow::CLAMP_TO_EDGE as i32,
    );

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
    use hyprwpe_core::pkg::Package;

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

    /// A texture name that already ends in an image extension must still get
    /// `.tex` tried. Creators embed their own assets, so a material naming
    /// `photo.jpeg` may have it stored as `materials/photo.jpeg.tex` - and
    /// deciding "this looks like an image, don't append an extension" dropped
    /// those layers silently.
    #[test]
    fn resolve_texture_entry_appends_tex_to_an_image_looking_name() {
        let pkg = make_test_pkg(&[("materials/photo.jpeg.tex", b"tex-data")]);
        assert_eq!(
            resolve_texture_entry(&pkg, "materials/photo.json", "photo.jpeg").as_deref(),
            Some("materials/photo.jpeg.tex")
        );

        // The name as written still wins when it is really there.
        let pkg = make_test_pkg(&[
            ("materials/bg.png", b"png"),
            ("materials/bg.png.tex", b"tex"),
        ]);
        assert_eq!(
            resolve_texture_entry(&pkg, "materials/bg.json", "bg.png").as_deref(),
            Some("materials/bg.png")
        );

        // And a bare name still resolves through `materials/`.
        let pkg = make_test_pkg(&[("materials/cat_rb.tex", b"tex")]);
        assert_eq!(
            resolve_texture_entry(&pkg, "materials/cat_rb.json", "cat_rb").as_deref(),
            Some("materials/cat_rb.tex")
        );
    }

    fn make_test_pkg(entries: &[(&str, &[u8])]) -> Resources {
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
        // No assets directory: a test package must resolve only from itself.
        Resources::from_package(Package::parse(bytes).unwrap(), None)
    }

    /// A material that declares no texture is a flat fill, and one that declares
    /// a render target samples the frame. Treating either as "unresolved" drops
    /// the layer, which is how the engine's own solid and full-screen layers
    /// disappeared from scenes.
    #[test]
    fn material_texture_classifies_flat_fill_and_render_target() {
        // Model -> material with a `flat` shader and no `textures` list.
        let model = r#"{"material": "materials/util/solidlayer.json"}"#;
        let mat = r#"{"passes": [{"shader": "flat"}]}"#;
        let pkg = make_test_pkg(&[
            ("models/util/solidlayer.json", model.as_bytes()),
            ("materials/util/solidlayer.json", mat.as_bytes()),
        ]);
        assert!(
            matches!(
                material_texture(&pkg, "models/util/solidlayer.json"),
                MaterialTexture::Unbound
            ),
            "a texture-less material paints the object's own colour"
        );

        // A material naming a render target must not be looked up as a file.
        let rt = r#"{"passes": [{"shader": "passthrough", "textures": ["_rt_FullFrameBuffer"]}]}"#;
        let pkg2 = make_test_pkg(&[("materials/util/fullscreenlayer.json", rt.as_bytes())]);
        assert!(
            matches!(
                material_texture(&pkg2, "materials/util/fullscreenlayer.json"),
                MaterialTexture::RenderTarget(t) if t == "_rt_FullFrameBuffer"
            ),
            "a `_rt_` name is the frame drawn so far, not a package entry"
        );

        // A name that is genuinely absent stays a drop.
        let named = r#"{"textures": ["nowhere"]}"#;
        let pkg3 = make_test_pkg(&[("materials/gone.json", named.as_bytes())]);
        assert!(matches!(
            material_texture(&pkg3, "materials/gone.json"),
            MaterialTexture::Missing
        ));
    }
}
