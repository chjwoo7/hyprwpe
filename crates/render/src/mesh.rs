//! Indexed-triangle mesh drawing for scene puppets.
//!
//! Scene image layers are single quads; a puppet model is an indexed triangle
//! mesh (see `docs/FORMATS.md`). This holds the GL objects for one such mesh and
//! draws it with the same orthographic projection the quad path uses.
//!
//! Skinning is done on the CPU (the mesh is small — a few thousand vertices) and
//! the deformed positions are re-uploaded each frame, so the shader stays the
//! simple position+uv one the rest of the scene renderer uses.

use crate::scene_layer::Mat4;
use anyhow::Result;
use glow::HasContext;

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

/// GL objects for one indexed mesh.
pub struct MeshRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    /// Interleaved `[x, y, u, v]` per vertex, re-uploaded when vertices deform.
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    loc_projection: Option<glow::UniformLocation>,
    loc_model: Option<glow::UniformLocation>,
    loc_color: Option<glow::UniformLocation>,
    loc_texture: Option<glow::UniformLocation>,
    index_count: i32,
    vertex_count: usize,
}

impl MeshRenderer {
    pub fn new(gl: &glow::Context) -> Result<Self> {
        unsafe {
            let vs = compile(gl, glow::VERTEX_SHADER, VERTEX_SHADER_SOURCE)?;
            let fs = compile(gl, glow::FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE)?;
            let program = link(gl, vs, fs)?;
            gl.delete_shader(vs);
            gl.delete_shader(fs);

            let loc_projection = gl.get_uniform_location(program, "uProjection");
            let loc_model = gl.get_uniform_location(program, "uModel");
            let loc_color = gl.get_uniform_location(program, "uColor");
            let loc_texture = gl.get_uniform_location(program, "uTexture");

            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("create mesh VAO: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("create mesh VBO: {e}"))?;
            let ebo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("create mesh EBO: {e}"))?;

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));

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

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);

            Ok(MeshRenderer {
                program,
                vao,
                vbo,
                ebo,
                loc_projection,
                loc_model,
                loc_color,
                loc_texture,
                index_count: 0,
                vertex_count: 0,
            })
        }
    }

    /// Upload a mesh: 2D positions and UVs, plus triangle indices.
    ///
    /// Indices outside the vertex range are dropped along with their whole
    /// triangle, so a truncated or mis-parsed buffer degrades to a missing
    /// triangle rather than reading out of bounds.
    pub fn upload(
        &mut self,
        gl: &glow::Context,
        positions: &[[f32; 2]],
        uvs: &[[f32; 2]],
        indices: &[u32],
    ) -> Result<()> {
        let n = positions.len().min(uvs.len());
        let mut interleaved = Vec::with_capacity(n * 4);
        for i in 0..n {
            interleaved.push(positions[i][0]);
            interleaved.push(positions[i][1]);
            interleaved.push(uvs[i][0]);
            interleaved.push(uvs[i][1]);
        }

        let mut safe = Vec::with_capacity(indices.len());
        for tri in indices.as_chunks::<3>().0 {
            if tri.iter().all(|&i| (i as usize) < n) {
                safe.extend_from_slice(tri);
            }
        }

        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            let bytes = std::slice::from_raw_parts(
                interleaved.as_ptr() as *const u8,
                interleaved.len() * std::mem::size_of::<f32>(),
            );
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);

            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ebo));
            let ibytes = std::slice::from_raw_parts(
                safe.as_ptr() as *const u8,
                safe.len() * std::mem::size_of::<u32>(),
            );
            gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, ibytes, glow::STATIC_DRAW);

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
        }

        self.index_count = safe.len() as i32;
        self.vertex_count = n;
        Ok(())
    }

    /// Replace the vertex positions (skinning output) without touching UVs or
    /// indices. A length mismatch is ignored: the caller keeps the old pose.
    pub fn update_positions(&mut self, gl: &glow::Context, positions: &[[f32; 2]]) {
        if positions.len() != self.vertex_count {
            return;
        }
        // Read-modify-write each vertex's xy while keeping its uv.
        unsafe {
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            for (i, p) in positions.iter().enumerate() {
                let off = (i * 4 * std::mem::size_of::<f32>()) as i32;
                let xy = [p[0], p[1]];
                let bytes = std::slice::from_raw_parts(
                    xy.as_ptr() as *const u8,
                    2 * std::mem::size_of::<f32>(),
                );
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, off, bytes);
            }
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.index_count == 0
    }

    pub fn draw(
        &self,
        gl: &glow::Context,
        projection: &Mat4,
        model: &Mat4,
        texture: glow::Texture,
        color: [f32; 4],
    ) {
        if self.index_count == 0 {
            return;
        }
        unsafe {
            gl.use_program(Some(self.program));
            if let Some(loc) = &self.loc_projection {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &projection.0);
            }
            if let Some(loc) = &self.loc_model {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &model.0);
            }
            if let Some(loc) = &self.loc_color {
                gl.uniform_4_f32(Some(loc), color[0], color[1], color[2], color[3]);
            }
            if let Some(loc) = &self.loc_texture {
                gl.uniform_1_i32(Some(loc), 0);
            }
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_elements(glow::TRIANGLES, self.index_count, glow::UNSIGNED_INT, 0);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }

    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_buffer(self.ebo);
        }
    }
}

unsafe fn compile(gl: &glow::Context, kind: u32, src: &str) -> Result<glow::Shader> {
    let shader = gl
        .create_shader(kind)
        .map_err(|e| anyhow::anyhow!("create mesh shader: {e}"))?;
    gl.shader_source(shader, src);
    gl.compile_shader(shader);
    if !gl.get_shader_compile_status(shader) {
        let log = gl.get_shader_info_log(shader);
        gl.delete_shader(shader);
        anyhow::bail!("mesh shader compilation failed:\n{log}");
    }
    Ok(shader)
}

unsafe fn link(gl: &glow::Context, vs: glow::Shader, fs: glow::Shader) -> Result<glow::Program> {
    let program = gl
        .create_program()
        .map_err(|e| anyhow::anyhow!("create mesh program: {e}"))?;
    gl.attach_shader(program, vs);
    gl.attach_shader(program, fs);
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        gl.delete_program(program);
        anyhow::bail!("mesh program link failed:\n{log}");
    }
    gl.detach_shader(program, vs);
    gl.detach_shader(program, fs);
    Ok(program)
}
