//! Wallpaper Engine effect shaders.
//!
//! An effect is **not** a built-in: a creator drops an `effects/<name>/effect.json`
//! into the package (or references one of the engine's own), and it points at a
//! material whose shader is plain GLSL carried in the same package. So
//! "implementing an effect" is really "hosting the engine's shader contract" —
//! do that once and every effect works, which is the only sane approach given the
//! corpus uses **91 distinct effects** (`docs/SCENE-COVERAGE.md`).
//!
//! The contract has three parts, all of which this module handles:
//!
//! 1. **A shader chain** — `effect.json` → `passes[].material` → the material's
//!    own `passes[].shader` (plus its texture slots and constant values).
//! 2. **A prelude and `#include`s** — every shader starts with `#include
//!    "common.h"`, and relies on engine built-ins (`texSample2D`, `mul`, …) that
//!    live nowhere in the file, so they come from here.
//! 3. **Uniform bindings** — each `uniform` carries a JSON comment naming the
//!    material constant it reads, e.g.
//!    `uniform float g_Scale; // {"material":"ui_editor_properties_ripple_scale","default":1}`.
//!    That comment is the whole mapping between the shader and the values the
//!    scene supplies, so it is parsed rather than guessed.

use anyhow::{Context, Result};
use std::collections::BTreeMap;

/// One resolved shader pass: the source plus everything needed to bind it.
#[derive(Debug, Clone)]
pub struct ShaderPass {
    /// Fragment (or vertex) shader source, includes expanded, prelude prepended.
    pub source: String,
    /// `"normal"`, `"additive"`, `"translucent"`, … as the material states it.
    pub blending: String,
    pub depth_test: bool,
    pub cull: bool,
    /// Texture slots in order; `g_Texture0` is slot 0. `None` is an empty slot,
    /// i.e. the pass samples whatever was rendered before it.
    pub textures: Vec<Option<String>>,
    /// Constant values the material itself sets, before the scene overrides them.
    pub constants: BTreeMap<String, serde_json::Value>,
    /// Whether this is a vertex or fragment stage.
    pub stage: Stage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
}

/// A uniform the shader reads, and the material constant that fills it.
#[derive(Debug, Clone, PartialEq)]
pub struct UniformBinding {
    /// GLSL name, e.g. `g_Scale`.
    pub name: String,
    /// GLSL type, e.g. `float` or `vec3`.
    pub ty: String,
    /// The material constant key from the JSON comment, when present.
    pub material: Option<String>,
    /// The JSON comment's `default`, when present.
    pub default: Option<serde_json::Value>,
    /// A combo name, when the uniform is a compile-time option rather than a value.
    pub combo: Option<String>,
    /// Marked `hidden` in the JSON comment (engine-provided, not editor-exposed).
    pub hidden: bool,
}

/// Parse a shader's `uniform` declarations and their JSON binding comments.
///
/// This is deliberately a textual scan of the declarations rather than a GLSL
/// parse: the comments are the contract, and they only ever appear on a
/// declaration line, which is exactly what is matched here.
pub fn uniform_bindings(source: &str) -> Vec<UniformBinding> {
    let mut out = Vec::new();
    for line in source.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("uniform ") else {
            continue;
        };
        // `uniform <type> <name>;` — stop at the first `;`.
        let Some(semi) = rest.find(';') else { continue };
        let decl = &rest[..semi];
        let mut parts = decl.split_whitespace();
        let (Some(ty), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        // A JSON comment may sit anywhere after the declaration on the same line.
        let comment_json = line[semi..].find('{').and_then(|brace| {
            let text = &line[semi + brace..];
            let end = text.rfind('}')?;
            serde_json::from_str::<serde_json::Value>(&text[..=end]).ok()
        });
        let get = |k: &str| comment_json.as_ref().and_then(|v| v.get(k)).cloned();
        out.push(UniformBinding {
            name: name.to_string(),
            ty: ty.to_string(),
            material: get("material").and_then(|v| v.as_str().map(String::from)),
            default: get("default"),
            combo: get("combo").and_then(|v| v.as_str().map(String::from)),
            hidden: get("hidden").and_then(|v| v.as_bool()).unwrap_or(false),
        });
    }
    out
}

/// Engine built-ins a bundled shader assumes exist.
///
/// These are not in any `#include` the creator ships — the engine injects them —
/// so a host must provide them or nothing compiles. `texSample2D` matches
/// `texture` on GLES3; `mul` is the HLSL row-vector multiply the shaders were
/// written with.
pub const PRELUDE: &str = r#"#version 300 es
precision highp float;
precision highp int;
precision highp sampler2D;

#define mul(a, b) ((b) * (a))
float saturate(float x) { return clamp(x, 0.0, 1.0); }
vec2 saturate(vec2 x) { return clamp(x, vec2(0.0), vec2(1.0)); }
vec3 saturate(vec3 x) { return clamp(x, vec3(0.0), vec3(1.0)); }
vec4 saturate(vec4 x) { return clamp(x, vec4(0.0), vec4(1.0)); }
float lerp(float a, float b, float t) { return mix(a, b, t); }
vec2 lerp(vec2 a, vec2 b, vec2 t) { return mix(a, b, t); }
vec3 lerp(vec3 a, vec3 b, vec3 t) { return mix(a, b, t); }
vec4 lerp(vec4 a, vec4 b, vec4 t) { return mix(a, b, t); }
vec2 atan2(vec2 a, vec2 b) { return atan(a, b); }
float mod2(float a, float b) { return mod(a, b); }
vec2 CAST2(vec2 v) { return v; }
vec3 CAST3(vec3 v) { return v; }
vec4 CAST4(vec3 v) { return vec4(v, 1.0); }
vec4 CAST4(vec4 v) { return v; }
vec2 rotateVec2(vec2 v, float r) {
    vec2 cs = vec2(cos(r), sin(r));
    return vec2(v.x * cs.x - v.y * cs.y, v.x * cs.y + v.y * cs.x);
}
float greyscale(vec3 color) { return dot(color, vec3(0.11, 0.59, 0.3)); }
vec3 hsv2rgb(vec3 c) {
    vec4 K = vec4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    vec3 p = abs(fract(c.xxx + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, 0.0, 1.0), c.y);
}
vec3 rgb2hsv(vec3 RGB) {
    vec4 P = (RGB.g < RGB.b) ? vec4(RGB.bg, -1.0, 2.0 / 3.0) : vec4(RGB.gb, 0.0, -1.0 / 3.0);
    vec4 Q = (RGB.r < P.x) ? vec4(P.xyw, RGB.r) : vec4(RGB.r, P.yzx);
    float C = Q.x - min(Q.w, Q.y);
    float H = abs((Q.w - Q.y) / (6.0 * C + 1e-10) + Q.z);
    vec3 HCV = vec3(H, C, Q.x);
    float S = HCV.y / (HCV.z + 1e-10);
    return vec3(HCV.x, S, HCV.z);
}
#define texSample2D(s, uv) texture(s, uv)
#define texSample2DLevel(s, uv, l) textureLod(s, uv, l)
"#;

/// Expand `#include "x.h"` recursively.
///
/// `lookup` resolves an include name to its text (the package first, then the
/// engine's assets). A missing include is reported rather than dropped, because a
/// shader that silently loses `common.h` fails to compile in a much less obvious
/// way. Each file is included at most once, and recursion is depth-bounded.
pub fn expand_includes(source: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let mut seen: BTreeMap<String, bool> = BTreeMap::new();
    expand_inner(source, lookup, &mut seen, 0)
}

fn expand_inner(
    source: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    seen: &mut BTreeMap<String, bool>,
    depth: u32,
) -> Result<String> {
    if depth > 16 {
        anyhow::bail!("include nesting too deep");
    }
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("#include") else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let rest = rest.trim_start();
        let Some(open) = rest.find(['"', '<']) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let open_char = rest.as_bytes()[open] as char;
        let close_char = if open_char == '"' { '"' } else { '>' };
        // The closing delimiter must be searched *after* the opening one.
        let Some(close_rel) = rest[open + 1..].find(close_char) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let name = &rest[open + 1..open + 1 + close_rel];
        if name.is_empty() {
            continue;
        }
        if seen.get(name).copied().unwrap_or(false) {
            continue;
        }
        seen.insert(name.to_string(), true);
        let body = lookup(name)
            .with_context(|| format!("shader includes {name:?}, which was not found"))?;
        out.push_str(&expand_inner(&body, lookup, seen, depth + 1)?);
        out.push('\n');
    }
    Ok(out)
}

/// Build the final, compilable source for one stage.
///
/// Exactly one `#version 300 es` line ends up first: any the shader states
/// itself is dropped (a `#version` anywhere but the top is a compile error), the
/// prelude follows without its own, and the shader body comes last. WE files also
/// start with a BOM and CRLF, which are normalised away.
pub fn assemble(source: &str, stage: Stage, defines: &[String]) -> String {
    let body = source.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let body: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("#version"))
        .collect::<Vec<_>>()
        .join("\n");

    let prelude_body: String = PRELUDE
        .lines()
        .filter(|l| !l.trim_start().starts_with("#version"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut out = String::with_capacity(body.len() + prelude_body.len() + 512);
    out.push_str("#version 300 es\n");
    if stage == Stage::Fragment {
        // WE writes `gl_FragColor`, which is not GLES3.
        out.push_str("#define gl_FragColor wp_FragColor\n");
    }
    out.push_str(&prelude_body);
    out.push('\n');
    for d in defines {
        out.push_str("#define ");
        out.push_str(d);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&body);
    out.push('\n');
    out
}

/// One pass of a material, as the material JSON states it.
#[derive(Debug, Clone, Default)]
pub struct MaterialPass {
    pub shader: String,
    pub blending: String,
    pub depth_test: bool,
    pub cull: bool,
    pub textures: Vec<Option<String>>,
    pub constants: BTreeMap<String, serde_json::Value>,
}

/// Parse a material JSON into its passes.
pub fn parse_material(json: &str) -> Result<Vec<MaterialPass>> {
    let json = json.strip_prefix('\u{feff}').unwrap_or(json);
    let v: serde_json::Value = serde_json::from_str(json).context("parsing material")?;
    let passes = v
        .get("passes")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for p in passes {
        let get_s = |k: &str| p.get(k).and_then(|v| v.as_str()).map(String::from);
        let textures = p
            .get("textures")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let constants = p
            .get("constantshadervalues")
            .and_then(|c| c.as_object())
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        out.push(MaterialPass {
            shader: get_s("shader").unwrap_or_default(),
            blending: get_s("blending").unwrap_or_else(|| "normal".into()),
            depth_test: !matches!(get_s("depthtest").as_deref(), Some("disabled")),
            cull: !matches!(get_s("cullmode").as_deref(), Some("nocull")),
            textures,
            constants,
        });
    }
    Ok(out)
}

/// The shader stage a filename denotes.
pub fn stage_of(path: &str) -> Stage {
    if path.ends_with(".vert") {
        Stage::Vertex
    } else {
        Stage::Fragment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_bindings_read_the_comment_contract() {
        let src = r#"
uniform sampler2D g_Texture0; // {"material":"ui_editor_properties_framebuffer","hidden":true}
uniform float g_Strength; // {"material":"ui_editor_properties_ripple_strength","default":0.1,"range":[0,1]}
uniform vec3 g_SpecularColor; // {"material":"ui_editor_properties_ripple_specular_color","default":"1 1 1","type":"color"}
uniform mat4 g_ModelViewProjectionMatrix;
"#;
        let b = uniform_bindings(src);
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].name, "g_Texture0");
        assert_eq!(b[0].ty, "sampler2D");
        assert!(b[0].hidden);
        assert_eq!(
            b[1].material.as_deref(),
            Some("ui_editor_properties_ripple_strength")
        );
        assert_eq!(b[1].default, Some(serde_json::json!(0.1)));
        assert_eq!(b[2].ty, "vec3");
        assert_eq!(
            b[2].material.as_deref(),
            Some("ui_editor_properties_ripple_specular_color")
        );
        // A built-in uniform has no material binding and is not hidden.
        assert_eq!(b[3].material, None);
        assert!(!b[3].hidden);
    }

    #[test]
    fn a_combo_uniform_is_detected() {
        let src = r#"uniform int g_Specular; // {"combo":"SPECULAR","type":"options","default":0}"#;
        let b = uniform_bindings(src);
        assert_eq!(b[0].combo.as_deref(), Some("SPECULAR"));
    }

    #[test]
    fn includes_expand_recursively_and_once() {
        let files = |n: &str| -> Option<String> {
            match n {
                "common.h" => Some("float helper() { return 1.0; }\n#include \"inner.h\"\n".into()),
                "inner.h" => Some("float inner() { return 2.0; }\n".into()),
                _ => None,
            }
        };
        let src = "#include \"common.h\"\nvoid main() {}\n#include \"common.h\"\n";
        let out = expand_includes(src, &files).unwrap();
        assert_eq!(out.matches("float helper()").count(), 1, "included once");
        assert_eq!(out.matches("float inner()").count(), 1);
        assert!(out.contains("void main"));
    }

    #[test]
    fn a_missing_include_is_an_error_not_a_silent_drop() {
        let none = |_: &str| None;
        assert!(expand_includes("#include \"common.h\"\n", &none).is_err());
    }

    #[test]
    fn an_include_cycle_terminates() {
        let files = |n: &str| -> Option<String> {
            match n {
                "a.h" => Some("#include \"b.h\"\n".into()),
                "b.h" => Some("#include \"a.h\"\n".into()),
                _ => None,
            }
        };
        // Each file is included once, so the cycle stops rather than recursing.
        assert!(expand_includes("#include \"a.h\"\n", &files).is_ok());
    }

    #[test]
    fn assembled_source_carries_the_prelude_and_version() {
        let out = assemble(
            "void main() {}",
            Stage::Fragment,
            &["COMBO_X 1".to_string()],
        );
        assert!(out.starts_with("#version 300 es"));
        assert!(out.contains("texSample2D"));
        assert!(out.contains("#define COMBO_X 1"));
        assert!(out.contains("gl_FragColor"));
        assert!(out.contains("void main()"));
    }

    #[test]
    fn a_shader_with_its_own_version_keeps_it_first() {
        let src = "#version 300 es\nvoid main() {}";
        let out = assemble(src, Stage::Fragment, &[]);
        // Exactly one #version, and it is the first line.
        assert_eq!(out.matches("#version").count(), 1);
        assert!(out.trim_start().starts_with("#version 300 es"));
    }

    #[test]
    fn material_passes_parse_with_textures_and_constants() {
        let m = r#"{"passes":[{
            "shader":"effects/waterripple","blending":"normal",
            "depthtest":"disabled","depthwrite":"disabled","cullmode":"nocull",
            "textures":[null,"effects/waterripplenormal"],
            "constantshadervalues":{"ui_editor_properties_ripple_scale":1.35}
        }]}"#;
        let p = parse_material(m).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].shader, "effects/waterripple");
        assert!(!p[0].depth_test);
        assert!(!p[0].cull);
        assert_eq!(
            p[0].textures,
            vec![None, Some("effects/waterripplenormal".into())]
        );
        assert!(p[0]
            .constants
            .contains_key("ui_editor_properties_ripple_scale"));
    }

    #[test]
    fn defaults_when_the_material_is_sparse() {
        let p = parse_material(r#"{"passes":[{"shader":"x"}]}"#).unwrap();
        assert_eq!(p[0].blending, "normal");
        assert!(p[0].depth_test);
        assert!(p[0].cull);
        assert!(p[0].textures.is_empty());
    }
}
