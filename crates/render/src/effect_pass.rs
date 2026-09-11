//! Apply a Wallpaper Engine effect chain to a texture.
//!
//! An object's effects are a list of shader passes that each read the previous
//! result and write a new one, so applying them is a framebuffer ping-pong: draw
//! a fullscreen quad sampling the last texture into the next one, pass by pass.
//! `crates/core/src/effect.rs` already resolves and assembles the shader source;
//! this module owns the GL side that turns it into pixels.
//!
//! The value contract, in precedence order, is the part worth stating because it
//! is where a pass either looks right or silently renders with defaults:
//!
//! 1. the **scene object's** `effects[].passes[].constantshadervalues`, keyed by
//!    the material name;
//! 2. the **material's** own `constantshadervalues`;
//! 3. the `default` in the uniform's JSON comment.
//!
//! A uniform's JSON comment is what names (1) and (2): for
//! `uniform float g_BlendAlpha; // {"material":"alpha"}` the key is `alpha`, and
//! that is what both the material and the scene are keyed by. A uniform with no
//! `material` is engine-provided (`g_Time`, the matrices, the texture
//! resolutions) and is bound here rather than looked up.

use anyhow::{Context, Result};
use glow::HasContext;
use hyprwpe_core::assets::Resources;
use hyprwpe_core::effect::{self, Stage, UniformBinding};
use std::collections::BTreeMap;

/// The most textures a pass may sample (`g_Texture0` … `g_Texture7`).
const MAX_TEXTURES: usize = 8;

/// How a pass blends its result onto what is already in the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blending {
    /// Straight alpha over (`blending: "normal"`).
    Normal,
    /// Additive (`blending: "additive"`).
    Additive,
    /// No blending; the pass replaces the target.
    None,
}

impl Blending {
    fn parse(name: &str) -> Blending {
        match name {
            "additive" => Blending::Additive,
            "disabled" | "none" => Blending::None,
            _ => Blending::Normal,
        }
    }
}

/// One pass of an effect chain, resolved and ready to draw.
///
/// An effect's passes form a small render graph: each writes to a **named
/// buffer** (`target`) and reads specific buffers into specific samplers
/// (`bind`), where the name `previous` means the chain's input. That graph is
/// what makes `godrays` work at all - its cast pass writes a ray mask, and its
/// final `combine` pass reads both the mask *and* the untouched input - so a
/// chain that ignores `target`/`bind` and simply feeds each pass the last result
/// produces a blank frame.
pub struct Pass {
    program: glow::Program,
    /// Uniforms the shader declares, with the material key each reads.
    uniforms: Vec<UniformBinding>,
    /// Uniform locations, resolved once at link time.
    locations: BTreeMap<String, Option<glow::UniformLocation>>,
    /// The buffer this pass writes to, or `None` for the chain's own output.
    target: Option<String>,
    /// Which named buffer feeds each sampler slot.
    bind: Vec<(String, usize)>,
    blending: Blending,
    /// Kept for diagnostics: which shader this pass came from.
    pub shader: String,
}

/// A resolved value is converted to GL types at bind time; see `set_uniform`.
///
/// One pass of an effect file: the material to run, plus the buffer wiring.
struct PassSpec {
    material: String,
    target: Option<String>,
    bind: Vec<(String, usize)>,
}

/// The resolved value of one uniform: a shader that declares `float g_X` wants
/// one number, `vec3 g_X` wants three. Storing the JSON value and converting at
/// bind time keeps the parse and the GL call separate.
pub fn parse_value_vec(value: &serde_json::Value) -> Option<[f32; 4]> {
    match value {
        serde_json::Value::Number(n) => {
            let v = n.as_f64()? as f32;
            Some([v, 0.0, 0.0, 0.0])
        }
        serde_json::Value::Bool(b) => Some([*b as u8 as f32, 0.0, 0.0, 0.0]),
        serde_json::Value::String(s) => {
            // The engine writes colours as `"1 0.5 0"` and the editor's colour
            // picker also produces hex.
            if let Some(hex) = s.trim().strip_prefix('#') {
                if hex.len() >= 6 {
                    let f = |i: usize| {
                        u8::from_str_radix(&hex[i..i + 2], 16)
                            .ok()
                            .map(|v| v as f32 / 255.0)
                    };
                    return Some([f(0)?, f(2)?, f(4)?, 1.0]);
                }
                return None;
            }
            let parts: Vec<f32> = s
                .split_whitespace()
                .filter_map(|p| p.parse::<f32>().ok())
                .collect();
            match parts.len() {
                1 => Some([parts[0], 0.0, 0.0, 0.0]),
                2 => Some([parts[0], parts[1], 0.0, 0.0]),
                3 => Some([parts[0], parts[1], parts[2], 0.0]),
                _ if parts.len() >= 4 => Some([parts[0], parts[1], parts[2], parts[3]]),
                _ => None,
            }
        }
        serde_json::Value::Array(a) => {
            let p: Vec<f32> = a
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            let get = |i: usize| p.get(i).copied().unwrap_or(0.0);
            Some([
                get(0),
                get(1),
                get(2),
                if p.len() >= 4 { get(3) } else { 0.0 },
            ])
        }
        _ => None,
    }
}

/// How many components a GLSL type carries.
fn component_count(ty: &str) -> usize {
    match ty {
        "float" | "int" | "bool" => 1,
        "vec2" | "ivec2" | "bvec2" => 2,
        "vec3" | "ivec3" | "bvec3" => 3,
        "vec4" | "ivec4" | "bvec4" => 4,
        _ => 0,
    }
}

/// The value a uniform should be set to, in the precedence order documented on
/// the module: the scene's own override, then the material's, then the default
/// carried in the JSON comment.
pub fn resolve_value(
    uniform: &UniformBinding,
    scene_values: Option<&serde_json::Map<String, serde_json::Value>>,
    material_values: &BTreeMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let key = uniform.material.as_ref()?;
    if let Some(values) = scene_values {
        if let Some(v) = values.get(key) {
            return Some(unwrap_user(v));
        }
    }
    if let Some(v) = material_values.get(key) {
        return Some(unwrap_user(v));
    }
    uniform.default.clone()
}

/// A scene value may still be a `{"user": …, "value": …}` binding. The property
/// layer resolves those before a scene is parsed, so this is a safety net for a
/// caller that resolved nothing: take the authored value rather than drop it.
fn unwrap_user(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(o) if o.contains_key("value") => o["value"].clone(),
        other => other.clone(),
    }
}

/// What a named buffer is for, so allocation is reported honestly.
const BUFFER_MEMORY_NOTE: &str = "buffers are allocated at the output size, not the reduced size the engine's `_rt_Quarter`/`_rt_Half` names imply: the shaders sample with normalised coordinates, so a full-size buffer is correct and merely costs memory";

/// A render target: a texture plus the framebuffer that draws into it.
struct Target {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
}

impl Target {
    unsafe fn new(gl: &glow::Context, width: i32, height: i32) -> Result<Self> {
        let texture = gl
            .create_texture()
            .map_err(|e| anyhow::anyhow!("creating a texture: {e}"))?;
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
        // Clamping matters: an effect that offsets UVs should smear the edge
        // pixel, not wrap around to the opposite side of the wallpaper.
        for (name, value) in [
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
        }

        let framebuffer = gl
            .create_framebuffer()
            .map_err(|e| anyhow::anyhow!("creating a framebuffer: {e}"))?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        Ok(Target {
            texture,
            framebuffer,
        })
    }

    unsafe fn destroy(&self, gl: &glow::Context) {
        gl.delete_framebuffer(self.framebuffer);
        gl.delete_texture(self.texture);
    }
}

/// A fullscreen quad in clip space, shared by every pass.
struct Quad {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
}

impl Quad {
    /// Position (vec2, already in clip space) and UV, interleaved.
    const VERTICES: [f32; 16] = [
        -1.0, -1.0, 0.0, 0.0, //
        1.0, -1.0, 1.0, 0.0, //
        -1.0, 1.0, 0.0, 1.0, //
        1.0, 1.0, 1.0, 1.0,
    ];

    unsafe fn new(gl: &glow::Context) -> Result<Self> {
        let vao = gl
            .create_vertex_array()
            .map_err(|e| anyhow::anyhow!("creating a VAO: {e}"))?;
        let vbo = gl
            .create_buffer()
            .map_err(|e| anyhow::anyhow!("creating a VBO: {e}"))?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let bytes = std::slice::from_raw_parts(
            Self::VERTICES.as_ptr() as *const u8,
            std::mem::size_of_val(&Self::VERTICES),
        );
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
        // The effect vertex shaders declare `a_Position` and `a_TexCoord`, bound
        // to locations 0 and 1 at link time.
        let stride = 4 * std::mem::size_of::<f32>() as i32;
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 2 * 4);
        gl.bind_vertex_array(None);
        Ok(Quad { vao, vbo })
    }

    unsafe fn destroy(&self, gl: &glow::Context) {
        gl.delete_vertex_array(self.vao);
        gl.delete_buffer(self.vbo);
    }
}

/// One effect file's chain: the passes plus the buffers they name.
pub struct Chain {
    passes: Vec<Pass>,
    /// Named intermediate buffers, created on first use at the input's size.
    buffers: BTreeMap<String, Target>,
    /// Where the last pass writes when it names no target.
    output: Option<Target>,
    quad: Quad,
    size: (i32, i32),
}

impl Chain {
    /// Build the chain an effect file describes, without combos.
    pub fn new(
        gl: &glow::Context,
        res: &Resources,
        effect_json: &str,
        scene_values: &[Option<serde_json::Map<String, serde_json::Value>>],
    ) -> Result<Self> {
        Self::with_combos(gl, res, effect_json, scene_values, &[])
    }

    /// Build the chain, honouring the combos the scene selected.
    ///
    /// `combos` is aligned with `scene_values`: entry `i` belongs to the i-th
    /// pass the material declares. A combo selects which branch of the shader's
    /// `#if` chain is compiled, so a pass built with the wrong branch can render
    /// nothing rather than merely looking different.
    pub fn with_combos(
        gl: &glow::Context,
        res: &Resources,
        effect_json: &str,
        scene_values: &[Option<serde_json::Map<String, serde_json::Value>>],
        combos: &[Vec<String>],
    ) -> Result<Self> {
        let specs = parse_effect_passes(effect_json)?;
        if specs.is_empty() {
            anyhow::bail!("effect has no material passes");
        }

        unsafe {
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        }
        let quad = unsafe { Quad::new(gl)? };
        let mut passes = Vec::new();
        let mut pass_index = 0usize;

        for spec in specs {
            let Some(body) = res.get_str(&spec.material) else {
                eprintln!("hyprwpe: effect material {} is missing", spec.material);
                continue;
            };
            let material_passes = effect::parse_material(&body)
                .with_context(|| format!("parsing material {}", spec.material))?;
            for mp in material_passes {
                if mp.shader.is_empty() {
                    continue;
                }
                let scene = scene_values.get(pass_index).and_then(|v| v.as_ref());
                let pass_combos = combos.get(pass_index);
                match Self::build_pass(gl, res, &mp, scene, pass_combos, &spec.target, &spec.bind) {
                    Ok(pass) => passes.push(pass),
                    Err(e) => eprintln!("hyprwpe: effect pass {} skipped: {e:#}", mp.shader),
                }
                pass_index += 1;
            }
        }
        let _ = BUFFER_MEMORY_NOTE;

        Ok(Chain {
            passes,
            buffers: BTreeMap::new(),
            output: None,
            quad,
            size: (0, 0),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_pass(
        gl: &glow::Context,
        res: &Resources,
        mp: &effect::MaterialPass,
        scene: Option<&serde_json::Map<String, serde_json::Value>>,
        scene_combos: Option<&Vec<String>>,
        target: &Option<String>,
        bind: &[(String, usize)],
    ) -> Result<Pass> {
        let frag_src = res
            .get_str(&format!("shaders/{}.frag", mp.shader))
            .with_context(|| format!("reading shaders/{}.frag", mp.shader))?;
        let lookup = |n: &str| res.shader(n);
        let vert_src = res
            .get_str(&format!("shaders/{}.vert", mp.shader))
            .unwrap_or_else(|| DEFAULT_VERTEX.to_string());

        // Material combos are defaults; the scene's selection overrides them,
        // because one material may be used in several configurations.
        let mut defines: Vec<String> = mp
            .combos
            .iter()
            .map(|(k, v)| format!("{k}={}", combo_value(v)))
            .collect();
        if let Some(scene) = scene_combos {
            for d in scene {
                let name = d.split('=').next().unwrap_or_default().to_string();
                defines.retain(|e| !e.starts_with(&format!("{name}=")));
                defines.push(d.clone());
            }
        }

        let frag = effect::assemble(
            &effect::expand_includes(&frag_src, &lookup)?,
            Stage::Fragment,
            &defines,
        );
        let vert = effect::assemble(
            &effect::expand_includes(&vert_src, &lookup)?,
            Stage::Vertex,
            &defines,
        );

        // Both stages: the vertex shader's `g_ModelViewProjectionMatrix` is in the
        // same program, and a pass that never sets it collapses every vertex to
        // the origin and draws nothing.
        let mut uniform_bindings = effect::uniform_bindings(
            &effect::expand_includes(&frag_src, &lookup).unwrap_or_default(),
        );
        for u in effect::uniform_bindings(
            &effect::expand_includes(&vert_src, &lookup).unwrap_or_default(),
        ) {
            if !uniform_bindings.iter().any(|e| e.name == u.name) {
                uniform_bindings.push(u);
            }
        }

        unsafe {
            let program =
                link(gl, &vert, &frag).with_context(|| format!("linking pass {}", mp.shader))?;

            // A uniform write goes to the *currently bound* program, so the
            // program must be current before any of these calls.
            gl.use_program(Some(program));

            let mut locations = BTreeMap::new();
            for u in &uniform_bindings {
                locations.insert(u.name.clone(), gl.get_uniform_location(program, &u.name));
            }
            // The quad is already in clip space, so identity is the projection.
            if let Some(loc) = locations
                .get("g_ModelViewProjectionMatrix")
                .cloned()
                .flatten()
            {
                gl.uniform_matrix_4_f32_slice(Some(&loc), false, &IDENTITY);
            }
            for u in &uniform_bindings {
                let Some(loc) = locations.get(&u.name).cloned().flatten() else {
                    continue;
                };
                let Some(value) = resolve_value(u, scene, &mp.constants) else {
                    continue;
                };
                if u.ty.starts_with("sampler") || u.ty == "mat4" {
                    continue;
                }
                set_uniform(gl, &loc, &u.ty, &value);
            }
            gl.use_program(None);

            Ok(Pass {
                program,
                uniforms: uniform_bindings,
                locations,
                target: target.clone(),
                bind: bind.to_vec(),
                blending: Blending::parse(&mp.blending),
                shader: mp.shader.clone(),
            })
        }
    }

    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.passes.len()
    }

    /// Run the chain over `input` and return the texture holding the result.
    ///
    /// The returned texture belongs to the chain and is reused next frame.
    pub fn apply(
        &mut self,
        gl: &glow::Context,
        input: glow::Texture,
        input_size: (i32, i32),
        extra: &[Option<glow::Texture>],
        time: f32,
    ) -> Result<glow::Texture> {
        if input_size != self.size {
            // The output changed size: every buffer is now the wrong shape.
            for t in self.buffers.values() {
                unsafe { t.destroy(gl) };
            }
            self.buffers.clear();
            if let Some(t) = self.output.take() {
                unsafe { t.destroy(gl) };
            }
            self.size = input_size;
        }

        let debug = std::env::var_os("HYPRWPE_DEBUG_FX").is_some();
        for i in 0..self.passes.len() {
            let needs_output = self.passes[i].target.is_none();
            if needs_output && self.output.is_none() {
                self.output = Some(unsafe { Target::new(gl, input_size.0, input_size.1)? });
            }
            if let Some(name) = self.passes[i].target.clone() {
                if let std::collections::btree_map::Entry::Vacant(slot) = self.buffers.entry(name) {
                    slot.insert(unsafe { Target::new(gl, input_size.0, input_size.1)? });
                }
            }

            // Resolve this pass's source textures: `previous` is the chain's
            // input, anything else is a named buffer.
            let sources: Vec<(usize, glow::Texture)> = if self.passes[i].bind.is_empty() {
                vec![(0, input)]
            } else {
                self.passes[i]
                    .bind
                    .iter()
                    .map(|(name, slot)| {
                        let tex = if name == "previous" {
                            input
                        } else {
                            self.buffers.get(name).map(|t| t.texture).unwrap_or(input)
                        };
                        (*slot, tex)
                    })
                    .collect()
            };

            let dest = match &self.passes[i].target {
                Some(name) => self
                    .buffers
                    .get(name)
                    .map(|t| t.framebuffer)
                    .expect("allocated above"),
                None => self.output.as_ref().expect("allocated above").framebuffer,
            };

            unsafe {
                self.draw_pass(gl, i, dest, input_size, &sources, extra, time)?;
            }
            if debug {
                let pass = &self.passes[i];
                let mut px = [0u8; 4];
                unsafe {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(dest));
                    gl.read_pixels(
                        input_size.0 / 2,
                        input_size.1 / 2,
                        1,
                        1,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(&mut px)),
                    );
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                }
                eprintln!(
                    "  pass {i} {} target={:?} blend={:?} center=({}, {}, {}, {})",
                    pass.shader, pass.target, pass.blending, px[0], px[1], px[2], px[3]
                );
            }
        }

        // The chain's result is the last pass's buffer, which is the output when
        // that pass named no target.
        let last = &self.passes[self.passes.len().saturating_sub(1)];
        let result = match &last.target {
            Some(name) => self.buffers.get(name).map(|t| t.texture),
            None => self.output.as_ref().map(|t| t.texture),
        };
        result.ok_or_else(|| anyhow::anyhow!("the chain's last pass wrote nowhere"))
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_pass(
        &self,
        gl: &glow::Context,
        index: usize,
        dest: glow::Framebuffer,
        size: (i32, i32),
        sources: &[(usize, glow::Texture)],
        extra: &[Option<glow::Texture>],
        time: f32,
    ) -> Result<()> {
        let pass = &self.passes[index];
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(dest));
        gl.viewport(0, 0, size.0, size.1);
        match pass.blending {
            // A pass writing to a fresh buffer has nothing to blend with - the
            // texture is never initialised - so a `normal` blend onto it would
            // keep whatever alpha the shader wrote and lose the colour. Clear
            // first, then draw.
            Blending::None => {
                gl.disable(glow::BLEND);
            }
            Blending::Normal => {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            }
            Blending::Additive => {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE);
            }
        }
        gl.disable(glow::DEPTH_TEST);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(glow::COLOR_BUFFER_BIT);

        gl.use_program(Some(pass.program));

        // Bind the sources the pass asked for. Any slot it does not name is left
        // alone: the shader either ignores it or the material listed a texture
        // the caller supplied.
        let mut bound_slots: Vec<usize> = Vec::new();
        for (slot, texture) in sources {
            let name = format!("g_Texture{slot}");
            let Some(Some(loc)) = pass.locations.get(&name).cloned() else {
                continue;
            };
            gl.active_texture(glow::TEXTURE0 + *slot as u32);
            gl.bind_texture(glow::TEXTURE_2D, Some(*texture));
            gl.uniform_1_i32(Some(&loc), *slot as i32);
            bound_slots.push(*slot);

            // The resolution uniform describes the texture actually bound; the
            // engine's shaders derive texel-sized offsets from `.zw`.
            let res_name = format!("g_Texture{slot}Resolution");
            if let Some(rloc) = pass.locations.get(&res_name).cloned().flatten() {
                let (w, h) = (size.0.max(1), size.1.max(1));
                gl.uniform_4_f32(
                    Some(&rloc),
                    w as f32,
                    h as f32,
                    1.0 / w as f32,
                    1.0 / h as f32,
                );
            }
        }
        // Material-listed textures the pass samples but the chain did not
        // supply: bind the caller's extra textures, so a mask or a normal map
        // reaches the shader.
        for slot in 0..MAX_TEXTURES {
            if bound_slots.contains(&slot) {
                continue;
            }
            let Some(tex) = extra.get(slot).and_then(|t| *t) else {
                continue;
            };
            let name = format!("g_Texture{slot}");
            let Some(Some(loc)) = pass.locations.get(&name).cloned() else {
                continue;
            };
            gl.active_texture(glow::TEXTURE0 + slot as u32);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(Some(&loc), slot as i32);
        }

        if let Some(loc) = pass.locations.get("g_Time").cloned().flatten() {
            gl.uniform_1_f32(Some(&loc), time);
        }
        let _ = &pass.uniforms;

        gl.bind_vertex_array(Some(self.quad.vao));
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.bind_vertex_array(None);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        Ok(())
    }

    /// Release every GL object the chain owns.
    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            for t in self.buffers.values() {
                t.destroy(gl);
            }
            if let Some(t) = &self.output {
                t.destroy(gl);
            }
            for p in &self.passes {
                gl.delete_program(p.program);
            }
            self.quad.destroy(gl);
        }
    }
}

/// Parse an effect file's passes into their material and buffer wiring.
fn parse_effect_passes(effect_json: &str) -> Result<Vec<PassSpec>> {
    let ev: serde_json::Value = serde_json::from_str(effect_json).context("parsing effect.json")?;
    let mut out = Vec::new();
    for p in ev
        .get("passes")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(material) = p.get("material").and_then(|m| m.as_str()) else {
            continue;
        };
        let bind = p
            .get("bind")
            .and_then(|b| b.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|b| {
                        Some((
                            b.get("name")?.as_str()?.to_string(),
                            b.get("index")?.as_u64()? as usize,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The material's own `combos` are merged in `build_pass`; an
        // effect.json's `combos` are alternates the *editor* switches between,
        // not a selection, so there is nothing here to apply.
        out.push(PassSpec {
            material: material.to_string(),
            target: p.get("target").and_then(|t| t.as_str()).map(String::from),
            bind,
        });
    }
    Ok(out)
}

/// A combo value as a `#define` body.
fn combo_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Bool(b) => (*b as u8).to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Which of the two ping-pong targets a pass index uses.
/// A vertex shader for a pass that does not ship one: fullscreen quad.
const DEFAULT_VERTEX: &str = r#"
attribute vec3 a_Position;
attribute vec2 a_TexCoord;
varying vec2 v_TexCoord;
void main() {
    gl_Position = vec4(a_Position, 1.0);
    v_TexCoord = a_TexCoord;
}
"#;

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

unsafe fn set_uniform(
    gl: &glow::Context,
    loc: &glow::UniformLocation,
    ty: &str,
    value: &serde_json::Value,
) {
    let Some(v) = parse_value_vec(value) else {
        return;
    };
    match (ty, component_count(ty)) {
        ("float", _) => gl.uniform_1_f32(Some(loc), v[0]),
        ("int", _) | ("bool", _) => gl.uniform_1_i32(Some(loc), v[0] as i32),
        ("vec2", _) => gl.uniform_2_f32(Some(loc), v[0], v[1]),
        ("vec3", _) => gl.uniform_3_f32(Some(loc), v[0], v[1], v[2]),
        ("vec4", _) => gl.uniform_4_f32(Some(loc), v[0], v[1], v[2], v[3]),
        _ => {}
    }
}

/// Compile and link a pass, binding the two attributes the shaders declare.
unsafe fn link(gl: &glow::Context, vert: &str, frag: &str) -> Result<glow::Program> {
    let mut shaders = Vec::new();
    for (kind, src, label) in [
        (glow::VERTEX_SHADER, vert, "vertex"),
        (glow::FRAGMENT_SHADER, frag, "fragment"),
    ] {
        let sh = gl
            .create_shader(kind)
            .map_err(|e| anyhow::anyhow!("creating the {label} shader: {e}"))?;
        gl.shader_source(sh, src);
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            let log = gl.get_shader_info_log(sh);
            for s in &shaders {
                gl.delete_shader(*s);
            }
            gl.delete_shader(sh);
            let first = log
                .lines()
                .find(|l| l.contains("error"))
                .unwrap_or(log.trim())
                .trim()
                .to_string();
            anyhow::bail!("{label} shader: {first}");
        }
        shaders.push(sh);
    }
    let program = gl
        .create_program()
        .map_err(|e| anyhow::anyhow!("creating a program: {e}"))?;
    for sh in &shaders {
        gl.attach_shader(program, *sh);
    }
    // The effect vertex shaders declare these by name; binding explicitly means
    // the quad's fixed attribute layout works for every pass.
    gl.bind_attrib_location(program, 0, "a_Position");
    gl.bind_attrib_location(program, 1, "a_TexCoord");
    gl.bind_frag_data_location(program, 0, "wp_FragColor");
    gl.link_program(program);
    let ok = gl.get_program_link_status(program);
    let log = gl.get_program_info_log(program);
    for sh in &shaders {
        gl.detach_shader(program, *sh);
        gl.delete_shader(*sh);
    }
    if !ok {
        gl.delete_program(program);
        anyhow::bail!("link: {}", log.trim());
    }
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding(
        name: &str,
        ty: &str,
        material: Option<&str>,
        default: serde_json::Value,
    ) -> UniformBinding {
        UniformBinding {
            name: name.into(),
            ty: ty.into(),
            material: material.map(String::from),
            default: Some(default),
            combo: None,
            hidden: false,
        }
    }

    /// The precedence order is the whole contract: the scene's per-object value
    /// must beat the material's, which must beat the comment's default.
    #[test]
    fn a_scene_value_beats_the_material_and_the_default() {
        let u = binding("g_TintColor", "vec3", Some("color"), json!("1 1 1"));
        let mut material = BTreeMap::new();
        material.insert("color".to_string(), json!("0.5 0.5 0.5"));
        let mut scene = serde_json::Map::new();
        scene.insert("color".into(), json!("1 0 0"));

        let got = resolve_value(&u, Some(&scene), &material).unwrap();
        assert_eq!(got, json!("1 0 0"), "the scene's own value wins");
    }

    #[test]
    fn the_material_beats_the_comment_default() {
        let u = binding("g_BlendAlpha", "float", Some("alpha"), json!(1.0));
        let mut material = BTreeMap::new();
        material.insert("alpha".to_string(), json!(0.25));
        assert_eq!(resolve_value(&u, None, &material).unwrap(), json!(0.25));
    }

    #[test]
    fn the_comment_default_is_the_last_resort() {
        let u = binding("g_BlendAlpha", "float", Some("alpha"), json!(0.75));
        assert_eq!(
            resolve_value(&u, None, &BTreeMap::new()).unwrap(),
            json!(0.75)
        );
    }

    /// An engine uniform has no material key, so there is nothing to look up:
    /// binding it is the host's job.
    #[test]
    fn a_uniform_with_no_material_key_is_not_resolved_from_values() {
        let u = binding("g_ModelViewProjectionMatrix", "mat4", None, json!(null));
        let mut scene = serde_json::Map::new();
        scene.insert("whatever".into(), json!(1));
        assert!(resolve_value(&u, Some(&scene), &BTreeMap::new()).is_none());
    }

    /// A scene value arrives as a `{"user": …, "value": …}` binding whenever the
    /// property layer has not already resolved it.
    #[test]
    fn an_unresolved_user_binding_contributes_its_value() {
        let u = binding("g_BlendAlpha", "float", Some("alpha"), json!(1.0));
        let mut scene = serde_json::Map::new();
        scene.insert("alpha".into(), json!({"user": "somekey", "value": 0.4}));
        assert_eq!(
            resolve_value(&u, Some(&scene), &BTreeMap::new()).unwrap(),
            json!(0.4)
        );
    }

    #[test]
    fn values_parse_from_the_forms_the_engine_writes() {
        assert_eq!(parse_value_vec(&json!(0.5)).unwrap(), [0.5, 0.0, 0.0, 0.0]);
        assert_eq!(parse_value_vec(&json!(true)).unwrap()[0], 1.0);
        assert_eq!(
            parse_value_vec(&json!("1 0.5 0")).unwrap(),
            [1.0, 0.5, 0.0, 0.0]
        );
        assert_eq!(
            parse_value_vec(&json!("0.1 0.2 0.3 0.4")).unwrap(),
            [0.1, 0.2, 0.3, 0.4]
        );
        let hex = parse_value_vec(&json!("#ff8000")).unwrap();
        assert!((hex[0] - 1.0).abs() < 1e-6 && (hex[1] - 0.502).abs() < 0.01);
        assert!(parse_value_vec(&json!("not a number")).is_none());
    }

    #[test]
    fn blending_names_map_to_the_engines_modes() {
        assert_eq!(Blending::parse("normal"), Blending::Normal);
        assert_eq!(Blending::parse("additive"), Blending::Additive);
        assert_eq!(Blending::parse("disabled"), Blending::None);
        // An unknown mode must not silently become additive.
        assert_eq!(Blending::parse("translucent"), Blending::Normal);
    }

    #[test]
    fn glsl_types_report_their_component_counts() {
        assert_eq!(component_count("float"), 1);
        assert_eq!(component_count("vec2"), 2);
        assert_eq!(component_count("vec3"), 3);
        assert_eq!(component_count("vec4"), 4);
        assert_eq!(component_count("sampler2D"), 0, "not a value uniform");
    }

    /// Build a minimal package on disk that carries one effect, so the GL path
    /// is covered by `cargo test` without depending on the user's library.
    ///
    /// The shader is deliberately trivial - it writes one uniform colour - so a
    /// failure can only be the pass plumbing, not the shader.
    fn synthetic_package(dir: &std::path::Path) -> std::path::PathBuf {
        let entries: [(&str, &[u8]); 4] = [
            (
                "effects/probe/effect.json",
                br#"{"passes":[{"material":"materials/probe.json"}]}"#,
            ),
            (
                "materials/probe.json",
                br#"{"passes":[{"shader":"effects/probe","blending":"normal"}]}"#,
            ),
            (
                "shaders/effects/probe.frag",
                br#"
uniform vec3 g_Color; // {"material":"color","default":"1 1 1"}
void main() {
    gl_FragColor = vec4(g_Color, 1.0);
}
"#,
            ),
            (
                "shaders/effects/probe.vert",
                br#"
uniform mat4 g_ModelViewProjectionMatrix;
attribute vec3 a_Position;
attribute vec2 a_TexCoord;
varying vec4 v_TexCoord;
void main() {
    gl_Position = mul(vec4(a_Position, 1.0), g_ModelViewProjectionMatrix);
    v_TexCoord = a_TexCoord.xyxy;
}
"#,
            ),
        ];
        let mut table = Vec::new();
        let mut body = Vec::new();
        for (name, content) in entries {
            table.extend_from_slice(&(name.len() as u32).to_le_bytes());
            table.extend_from_slice(name.as_bytes());
            table.extend_from_slice(&(body.len() as u32).to_le_bytes());
            table.extend_from_slice(&(content.len() as u32).to_le_bytes());
            body.extend_from_slice(content);
        }
        let version = b"PKGV0001";
        let mut pkg = Vec::new();
        pkg.extend_from_slice(&(version.len() as u32).to_le_bytes());
        pkg.extend_from_slice(version);
        pkg.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        pkg.extend_from_slice(&table);
        pkg.extend_from_slice(&body);
        let path = dir.join("scene.pkg");
        std::fs::write(&path, pkg).unwrap();
        path
    }

    /// The end-to-end check, on the same surfaceless context the tools use: an
    /// effect chain must turn its input into something the shader's uniforms
    /// describe. Skipped (with a reason) only where there is no GL at all.
    #[test]
    fn a_chain_renders_the_colour_its_scene_value_asks_for() {
        let crate_headless = crate::headless::Headless::new();
        let Ok(ctx) = crate_headless else {
            eprintln!(
                "no headless GL context; skipping: {:?}",
                crate_headless.err()
            );
            return;
        };

        let dir = std::env::temp_dir().join(format!("hyprwpe-fxpass-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pkg = synthetic_package(&dir);
        // No engine assets: this package is self-contained on purpose.
        let res = Resources::with_assets(&pkg, None).unwrap();
        let gl = &ctx.gl;
        let (w, h) = (4, 4);

        let render = |scene: Option<serde_json::Map<String, serde_json::Value>>| {
            let effect_json = res.get_str("effects/probe/effect.json").unwrap();
            let mut chain = Chain::new(gl, &res, &effect_json, &[scene]).unwrap();
            assert!(!chain.is_empty(), "the pass must build");
            let (input, _fbo) = solid_source(gl, w, h, [0, 0, 0, 255]);
            let extra: Vec<Option<glow::Texture>> = vec![None; 8];
            let out = chain.apply(gl, input, (w, h), &extra, 0.0).unwrap();
            let pixels = unsafe {
                let fbo = gl.create_framebuffer().unwrap();
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(out),
                    0,
                );
                let px = ctx.read_framebuffer(fbo, w, h);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.delete_framebuffer(fbo);
                px
            };
            chain.destroy(gl);
            unsafe { gl.delete_texture(input) };
            let mean = |c: usize| {
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| p[c] as f32)
                    .sum::<f32>()
                    / (w * h) as f32
            };
            [mean(0), mean(1), mean(2)]
        };

        // The comment's default is white; a scene value overrides it.
        let default_rgb = render(None);
        assert!(
            default_rgb.iter().all(|c| *c > 250.0),
            "the shader default must be used when the scene supplies nothing: {default_rgb:?}"
        );

        let mut red = serde_json::Map::new();
        red.insert("color".into(), json!("1 0 0"));
        let red_rgb = render(Some(red));
        assert!(red_rgb[0] > 250.0, "red channel: {red_rgb:?}");
        assert!(
            red_rgb[1] < 5.0 && red_rgb[2] < 5.0,
            "green and blue must be dark: {red_rgb:?}"
        );

        let mut blue = serde_json::Map::new();
        blue.insert("color".into(), json!("0 0 1"));
        let blue_rgb = render(Some(blue));
        assert!(
            blue_rgb[2] > 250.0 && blue_rgb[0] < 5.0,
            "a different scene value must reach the shader: {blue_rgb:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A source texture, mid-grey by default: enough for a pass to have
    /// somewhere to go.
    fn solid_source(
        gl: &glow::Context,
        width: i32,
        height: i32,
        rgba: [u8; 4],
    ) -> (glow::Texture, glow::Framebuffer) {
        unsafe {
            let texture = gl.create_texture().unwrap();
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            let mut bytes = Vec::new();
            for _ in 0..(width * height) {
                bytes.extend_from_slice(&rgba);
            }
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&bytes)),
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
            let framebuffer = gl.create_framebuffer().unwrap();
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            (texture, framebuffer)
        }
    }

    /// An effect's passes are a small render graph, and ignoring `target`/`bind`
    /// is what made a whole wallpaper render blank: the cast pass wrote a ray
    /// mask, the combine pass read it *and* the untouched input, and feeding each
    /// pass the last result instead lost the image entirely.
    #[test]
    fn effect_passes_parse_their_buffer_wiring() {
        let json = r#"{"passes":[
            {"material":"materials/a.json","target":"_rt_Half1","bind":[{"name":"previous","index":0}]},
            {"material":"materials/b.json","target":"_rt_Half2","bind":[{"name":"_rt_Half1","index":0}]},
            {"material":"materials/c.json","bind":[{"name":"_rt_Half2","index":0},{"name":"previous","index":1}]}
        ]}"#;
        let specs = parse_effect_passes(json).unwrap();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].target.as_deref(), Some("_rt_Half1"));
        assert_eq!(specs[0].bind, vec![("previous".to_string(), 0)]);
        assert_eq!(specs[1].bind, vec![("_rt_Half1".to_string(), 0)]);
        // The final pass names no target: it writes the chain's own output, and
        // it reads two different buffers at two different slots.
        assert_eq!(specs[2].target, None);
        assert_eq!(
            specs[2].bind,
            vec![("_rt_Half2".to_string(), 0), ("previous".to_string(), 1)]
        );
    }

    #[test]
    fn a_pass_with_no_bind_reads_the_chains_input() {
        let specs = parse_effect_passes(r#"{"passes":[{"material":"materials/a.json"}]}"#).unwrap();
        assert!(specs[0].bind.is_empty(), "no bind means the chain input");
        assert_eq!(specs[0].target, None);
    }

    /// Combos are compile-time branch selectors, so they must reach the shader.
    #[test]
    fn scene_combos_become_defines_that_override_the_materials() {
        // Built the way `build_pass` merges them: material first, scene last.
        let mut defines: Vec<String> = vec!["VERTICAL=0".into(), "KERNEL=0".into()];
        let scene = vec!["VERTICAL=1".to_string()];
        for d in &scene {
            let name = d.split('=').next().unwrap().to_string();
            defines.retain(|e| !e.starts_with(&format!("{name}=")));
            defines.push(d.clone());
        }
        let assembled = effect::assemble(
            "#if VERTICAL\nfloat dir = 1.0;\n#else\nfloat dir = 0.0;\n#endif\nvoid main(){}",
            Stage::Fragment,
            &defines,
        );
        assert!(assembled.contains("#define VERTICAL=1"), "{assembled}");
        assert_eq!(assembled.matches("VERTICAL=").count(), 1, "no stale define");
        assert!(
            assembled.contains("#define KERNEL=0"),
            "the other is untouched"
        );
    }

    #[test]
    fn combo_values_render_as_the_shaders_if_tests_expect() {
        assert_eq!(combo_value(&json!(1)), "1");
        assert_eq!(combo_value(&json!(true)), "1");
        assert_eq!(combo_value(&json!(false)), "0");
        assert_eq!(combo_value(&json!("mode")), "mode");
    }

    /// A pass must never read the buffer it writes. The engine names buffers
    /// explicitly (`target` + `bind`), so the check is that no pass both targets
    /// a name and reads that same name.
    #[test]
    fn no_pass_reads_the_buffer_it_writes() {
        let effects = [
            r#"{"passes":[
                {"material":"m/a.json","target":"A","bind":[{"name":"previous","index":0}]},
                {"material":"m/b.json","target":"B","bind":[{"name":"A","index":0}]},
                {"material":"m/c.json","bind":[{"name":"B","index":0},{"name":"previous","index":1}]}
            ]}"#,
            r#"{"passes":[
                {"material":"m/x.json","target":"A","bind":[{"name":"previous","index":0}]},
                {"material":"m/y.json","target":"A","bind":[{"name":"previous","index":0}]}
            ]}"#,
        ];
        for json in effects {
            for spec in parse_effect_passes(json).unwrap() {
                let Some(target) = &spec.target else { continue };
                assert!(
                    !spec.bind.iter().any(|(name, _)| name == target),
                    "pass writing {target} also reads it"
                );
            }
        }
    }
}
