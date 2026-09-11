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
/// These are **not** in any `#include` the creator ships — the engine injects
/// them — so a host must provide them or nothing compiles. Everything here is a
/// **macro**, in three deliberate groups:
///
/// - **HLSL names**, because the shaders are written in Wallpaper Engine's
///   HLSL-ish GLSL: `frac`, `saturate`, `lerp`, `mul`, `atan2`, `mod2`.
/// - **Type-generic forms**, so no overload can collide with the shader's own
///   definitions (`saturate(x)` is `clamp(x, 0.0, 1.0)`, which GLSL accepts for
///   any `genType`). This is not cosmetic: the engine's own `common.h` defines
///   `hsv2rgb`, `rgb2hsv`, `rotateVec2` and `greyscale` as *functions*, and a
///   function of the same name in the prelude is a hard
///   "function is already defined" error on the driver. Those four therefore
///   live only in `common.h` and are **not** repeated here.
/// - **Sampler aliases**, since GLES3 spells it `texture`/`textureLod`.
///
/// Every definition is `#ifndef`-guarded, so a shader or include that provides
/// its own cannot conflict.
pub const PRELUDE: &str = r#"
#ifndef mul
#define mul(a, b) ((b) * (a))
#endif
#ifndef frac
#define frac(x) fract(x)
#endif
#ifndef saturate
#define saturate(x) clamp(x, 0.0, 1.0)
#endif
#ifndef lerp
#define lerp(a, b, t) mix(a, b, t)
#endif
#ifndef atan2
#define atan2(a, b) atan(a, b)
#endif
#ifndef mod2
#define mod2(a, b) mod(a, b)
#endif
// `CASTn` promotes to an n-vector, so it must broadcast: the corpus calls
// `CAST2(1.409)` on a scalar inside a `vec2` expression, which only compiles if
// these are overloaded constructors rather than a plain pass-through.
// Integer overloads matter as much as the float ones: the corpus calls
// `CAST2(3)`, and GLSL will not implicitly widen `int` to `float` the way HLSL
// does, so a missing overload is a hard error rather than a quiet conversion.
vec2 CAST2(int v) { return vec2(float(v)); }
vec2 CAST2(float v) { return vec2(v); }
vec2 CAST2(vec2 v) { return v; }
vec3 CAST3(int v) { return vec3(float(v)); }
vec3 CAST3(float v) { return vec3(v); }
vec3 CAST3(vec3 v) { return v; }
vec4 CAST4(int v) { return vec4(float(v)); }
vec4 CAST4(float v) { return vec4(v); }
vec4 CAST4(vec3 v) { return vec4(v, 1.0); }
vec4 CAST4(vec4 v) { return v; }
#ifndef texSample2D
#define texSample2D(s, uv) texture(s, uv)
#endif
#ifndef texSample2DLevel
#define texSample2DLevel(s, uv, l) textureLod(s, uv, l)
#endif
#ifndef texSample2DLod
#define texSample2DLod(s, uv, l) textureLod(s, uv, l)
#endif
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

/// Combo macros a stage needs defined.
///
/// A shader guards its variants with `#if SOME_COMBO`, naming the combo in each
/// uniform's JSON comment (`{"combo":"SOME_COMBO"}`). GLSL ES rejects an `#if`
/// over an undefined name where HLSL treated it as 0, so a host that does not
/// select a combo must still define it. `0` is the honest default: it is what
/// "this option is off" means, and the shader's own `#else` branch then applies.
pub fn combo_defines(source: &str) -> Vec<String> {
    let mut names: Vec<String> = uniform_bindings(source)
        .into_iter()
        .filter_map(|u| u.combo)
        .collect();
    // Also catch combos referenced in `#if` but not declared by a uniform: some
    // effects branch on a name the material sets instead.
    for line in source.lines() {
        let line = line.trim_start();
        let rest = match line
            .strip_prefix("#if ")
            .or_else(|| line.strip_prefix("#if\t"))
        {
            Some(r) => r.trim(),
            None => continue,
        };
        // `#if NAME`, `#if NAME == 1`, `#if !NAME`, `#if defined(NAME)`
        for token in rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if token.is_empty() || token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                continue;
            }
            // Skip GLSL's own defined/integer-constant helpers.
            if matches!(token, "defined" | "true" | "false") {
                continue;
            }
            names.push(token.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|n| format!("#ifndef {n}\n#define {n} 0\n#endif"))
        .collect()
}

/// Build the final, compilable source for one stage.
///
/// The pieces go in the only order GLSL accepts and in the order the engine's
/// own files assume:
///
/// 1. `#version` — first line, once. Any the shader states is dropped, because a
///    `#version` anywhere but the top is itself a compile error.
/// 2. Precision and the fragment output. The shaders write `gl_FragColor`, which
///    is not GLES 3, so it is aliased to a declared `out`.
/// 3. The prelude macros and this stage's combo defaults.
/// 4. The body, with `#include`s already expanded.
///
/// WE files start with a BOM and use CRLF; both are normalised away, since a
/// stray `\r` inside a `#define` is a genuinely confusing error.
/// The sampler uniforms the engine declares for every shader.
///
/// A shader's `#include`d helper may *use* `g_Texture0` while the shader itself
/// declares it further down the file - `common_blur.h` does exactly that - and
/// GLSL requires declaration before use, so the host declares the whole set
/// first. Any declaration the shader repeats is removed by
/// [`strip_texture_uniforms`], because a duplicate uniform is an error.
pub const TEXTURE_UNIFORMS: usize = 8;

fn texture_uniform_block() -> String {
    let mut out = String::new();
    for i in 0..TEXTURE_UNIFORMS {
        out.push_str(&format!(
            "uniform sampler2D g_Texture{i};\nuniform vec4 g_Texture{i}Resolution;\n"
        ));
    }
    out
}

/// Drop the shader's own declarations of the injected samplers.
///
/// Matching on the declaration line is safe here: the injected set has one fixed
/// type per name, so a removed line and the header's version are equivalent.
fn strip_texture_uniforms(body: &str) -> String {
    body.lines()
        .filter(|line| {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("uniform ") else {
                return true;
            };
            let mut parts = rest.split_whitespace();
            let (Some(_ty), Some(name)) = (parts.next(), parts.next()) else {
                return true;
            };
            let name = name.trim_end_matches(';');
            let injected = ["sampler2D", "vec4"].contains(&_ty);
            let is_g_texture = name.starts_with("g_Texture")
                && name[9..].chars().next().is_some_and(|c| c.is_ascii_digit());
            !(injected && is_g_texture)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn assemble(source: &str, stage: Stage, defines: &[String]) -> String {
    let body = source.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let body: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("#version"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = strip_texture_uniforms(&body);

    let mut out = String::with_capacity(body.len() + PRELUDE.len() + 512);
    out.push_str("#version 300 es\n");
    if stage == Stage::Fragment {
        out.push_str("precision highp float;\nprecision highp int;\n");
        out.push_str("layout(location = 0) out vec4 wp_FragColor;\n");
        out.push_str("#define gl_FragColor wp_FragColor\n");
    }
    out.push_str(PRELUDE);
    out.push('\n');
    // Both stages: a vertex shader's include may sample just as a fragment
    // shader's does (`blend.vert` reads `g_Texture1Resolution`).
    out.push_str(&texture_uniform_block());
    for d in combo_defines(&body) {
        out.push_str(&d);
        out.push('\n');
    }
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

    /// The GPU found this: a helper `common.h` defines must not be repeated in
    /// the prelude, or the driver reports "function is already defined".
    #[test]
    fn the_prelude_does_not_redefine_the_engines_own_helpers() {
        for name in ["hsv2rgb", "rgb2hsv", "rotateVec2", "greyscale"] {
            assert!(
                !PRELUDE.contains(name),
                "{name} is defined by the engine's common.h; repeating it fails to compile"
            );
        }
        // ...while the HLSL names the engine injects and no include provides
        // must be present.
        for name in ["frac", "saturate", "lerp", "mul", "texSample2D"] {
            assert!(PRELUDE.contains(name), "{name} must come from the host");
        }
    }

    /// GLSL ES rejects `#if` over an undefined name where HLSL read it as 0.
    #[test]
    fn combo_macros_are_defined_for_the_shaders_if_blocks() {
        let src = "uniform int g_Specular; // {\"combo\":\"SPECULAR\"}\n#if NORMALMAP\n#endif\n";
        let defs = combo_defines(src);
        assert!(defs.iter().any(|d| d.contains("SPECULAR")), "{defs:?}");
        assert!(defs.iter().any(|d| d.contains("NORMALMAP")), "{defs:?}");
        // Each is guarded so a shader that defines its own still wins.
        assert!(defs.iter().all(|d| d.starts_with("#ifndef ")), "{defs:?}");
        assert!(defs.iter().all(|d| d.ends_with("#endif")));
    }

    /// A shader's `#include`d helper may use `g_Texture0` before the shader
    /// declares it, which GLSL rejects - so the host declares the set first and
    /// the shader's own duplicate is removed.
    #[test]
    fn sampler_uniforms_are_injected_before_the_body_and_not_duplicated() {
        let src =
            "uniform sampler2D g_Texture0; // {\"material\":\"framebuffer\"}\nvoid main() {}\n";
        let out = assemble(src, Stage::Fragment, &[]);
        let declarations = out.matches("uniform sampler2D g_Texture0;").count();
        assert_eq!(declarations, 1, "declared exactly once:\n{out}");
        // The injected block comes before the body, so an include can use it.
        let decl_at = out.find("uniform sampler2D g_Texture0;").unwrap();
        let main_at = out.find("void main()").unwrap();
        assert!(decl_at < main_at, "the sampler must precede its use");
        // Every resolution uniform the shaders read is present too.
        assert!(out.contains("uniform vec4 g_Texture0Resolution;"));
    }

    #[test]
    fn a_vertex_stage_gets_the_samplers_as_well() {
        let out = assemble("void main() {}", Stage::Vertex, &[]);
        assert!(out.contains("uniform sampler2D g_Texture0;"));
        assert!(out.contains("uniform vec4 g_Texture1Resolution;"));
    }

    /// `CASTn` must broadcast, because the corpus calls it on a bare integer.
    #[test]
    fn cast_helpers_accept_integers_and_floats_and_vectors() {
        // The corpus calls these on bare integers (`CAST2(3)`), on floats and on
        // vectors, and GLSL resolves overloads by exact type - so all three must
        // exist or the call is a hard error.
        for decl in [
            "vec2 CAST2(int v)",
            "vec2 CAST2(float v)",
            "vec2 CAST2(vec2 v)",
            "vec3 CAST3(int v)",
            "vec3 CAST3(float v)",
            "vec4 CAST4(int v)",
            "vec4 CAST4(float v)",
            "vec4 CAST4(vec3 v)",
            "vec4 CAST4(vec4 v)",
        ] {
            assert!(PRELUDE.contains(decl), "missing overload: {decl}");
        }
    }

    #[test]
    fn the_assembled_header_is_the_only_version_and_has_one_output() {
        let out = assemble("#version 300 es\nvoid main() {}", Stage::Fragment, &[]);
        assert_eq!(out.matches("#version").count(), 1);
        assert!(out.trim_start().starts_with("#version 300 es"));
        assert_eq!(out.matches("out vec4 wp_FragColor;").count(), 1);
        assert!(out.contains("#define gl_FragColor wp_FragColor"));
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
