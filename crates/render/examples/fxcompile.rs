//! Compile every effect shader in the corpus, headlessly.
//!
//! `validate_effects` proves the *host* side of the shader contract: the chain
//! resolves, includes expand, sources assemble. It cannot tell you whether the
//! result is valid GLSL — only a driver can. This example gets a real GLES 3
//! context through **surfaceless EGL** (no window, no compositor, nothing on
//! screen), compiles each assembled stage, and prints the driver's own error log
//! for anything that fails.
//!
//! That distinction matters: an assembled source can be structurally right and
//! still be rejected for a missing uniform type, a reserved word, or a prelude
//! that shadows a built-in. This is the check that catches those.
//!
//! Usage: cargo run -p hyprwpe-render --example fxcompile [-- <workshop-dir>]

use glow::HasContext;
use hyprwpe_core::assets::Resources;
use hyprwpe_core::effect::{self, Stage};
use hyprwpe_render::headless::Headless;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Compile one stage, returning the driver's log on failure.
fn compile(gl: &glow::Context, kind: u32, source: &str) -> Result<(), String> {
    unsafe {
        let shader = gl
            .create_shader(kind)
            .map_err(|e| format!("create_shader: {e}"))?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        let ok = gl.get_shader_compile_status(shader);
        let log = gl.get_shader_info_log(shader);
        gl.delete_shader(shader);
        if ok {
            Ok(())
        } else {
            Err(log)
        }
    }
}

/// Compile both stages of a pass and link them into a program.
///
/// Compiling is only half of what a render pass needs. Linking the stages
/// together is what catches an interface the vertex and fragment shaders
/// disagree about - a `varying` never written, a mismatched type - which no
/// single-stage compile can see.
fn link(gl: &glow::Context, vert: &str, frag: &str) -> Result<(), String> {
    unsafe {
        let mut shaders = Vec::new();
        for (kind, src, label) in [
            (glow::VERTEX_SHADER, vert, "vertex"),
            (glow::FRAGMENT_SHADER, frag, "fragment"),
        ] {
            let sh = gl
                .create_shader(kind)
                .map_err(|e| format!("create_shader({label}): {e}"))?;
            gl.shader_source(sh, src);
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                let log = gl.get_shader_info_log(sh);
                for s in &shaders {
                    gl.delete_shader(*s);
                }
                gl.delete_shader(sh);
                return Err(format!("{label}: {}", log.trim()));
            }
            shaders.push(sh);
        }
        let program = gl
            .create_program()
            .map_err(|e| format!("create_program: {e}"))?;
        for sh in &shaders {
            gl.attach_shader(program, *sh);
        }
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
        gl.delete_program(program);
        if ok {
            Ok(())
        } else {
            Err(format!("link: {}", log.trim()))
        }
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("HYPRWPE_WORKSHOP").ok())
        .unwrap_or_else(|| "~/.steam/root/steamapps/workshop/content/431960".into());
    let root = PathBuf::from(expand(&root));

    let ctx = match Headless::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no headless GL context: {e:#}");
            std::process::exit(2);
        }
    };
    let gl = &ctx.gl;
    println!("GL context: {}", ctx.version());

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read workshop dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").is_file())
        .collect();
    dirs.sort();

    let mut effects: BTreeMap<String, (usize, Option<String>)> = BTreeMap::new();
    let mut stages_ok = 0usize;
    let mut stages_failed = 0usize;
    let mut programs_attempted = 0usize;
    let mut programs_linked = 0usize;
    let mut programs_failed = 0usize;

    for dir in &dirs {
        let Ok(res) = Resources::open(&dir.join("scene.pkg")) else {
            continue;
        };
        let Some(scene_json) = res.get_str("scene.json") else {
            continue;
        };
        let Ok(scene) = hyprwpe_core::scene::Scene::from_json_str(&scene_json) else {
            continue;
        };
        for obj in &scene.objects {
            for eff in &obj.effects {
                let Some(file) = eff.file.as_ref() else {
                    continue;
                };
                let entry = effects.entry(file.clone()).or_insert((0, None));
                entry.0 += 1;
                if entry.1.is_some() {
                    continue;
                }
                let Some(body) = res.get_str(file) else {
                    continue;
                };
                let Ok(ev) = serde_json::from_str::<serde_json::Value>(&body) else {
                    continue;
                };
                let mats: Vec<String> = ev
                    .get("passes")
                    .and_then(|p| p.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.get("material").and_then(|m| m.as_str()))
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut first_error: Option<String> = None;
                'mat: for mat in mats {
                    let Some(mb) = res.get_str(&mat) else {
                        continue;
                    };
                    let Ok(passes) = effect::parse_material(&mb) else {
                        continue;
                    };
                    for pass in passes {
                        if pass.shader.is_empty() {
                            continue;
                        }
                        // Build both stages, then link them: a render pass needs
                        // a program, and linking is what catches an interface the
                        // two stages disagree about.
                        let mut built: Vec<(Stage, String)> = Vec::new();
                        let mut stage_err: Option<String> = None;
                        for ext in ["frag", "vert"] {
                            let path = format!("shaders/{}.{ext}", pass.shader);
                            let Some(src) = res.get_str(&path) else {
                                continue;
                            };
                            let stage = if ext == "frag" {
                                Stage::Fragment
                            } else {
                                Stage::Vertex
                            };
                            let lookup = |n: &str| res.shader(n);
                            let expanded = match effect::expand_includes(&src, &lookup) {
                                Ok(e) => e,
                                Err(e) => {
                                    stages_failed += 1;
                                    stage_err = Some(format!("{path}: {e}"));
                                    break;
                                }
                            };
                            let assembled = effect::assemble(&expanded, stage, &[]);
                            let kind = if stage == Stage::Fragment {
                                glow::FRAGMENT_SHADER
                            } else {
                                glow::VERTEX_SHADER
                            };
                            match compile(gl, kind, &assembled) {
                                Ok(()) => {
                                    stages_ok += 1;
                                    built.push((stage, assembled));
                                }
                                Err(log) => {
                                    stages_failed += 1;
                                    if std::env::var_os("FXDUMP").is_some() {
                                        let base = path.replace('/', "_");
                                        let out = format!("/tmp/fx_{base}.glsl");
                                        let _ = std::fs::write(&out, &assembled);
                                        eprintln!("dumped {out}");
                                    }
                                    stage_err =
                                        Some(format!("{path}:{}", error_lines(&log, &assembled)));
                                    break;
                                }
                            }
                        }
                        if let Some(e) = stage_err {
                            first_error = Some(e);
                            break 'mat;
                        }
                        let frag = built
                            .iter()
                            .find(|(s, _)| *s == Stage::Fragment)
                            .map(|(_, src)| src.clone());
                        let vert = built
                            .iter()
                            .find(|(s, _)| *s == Stage::Vertex)
                            .map(|(_, src)| src.clone());
                        if let (Some(frag), Some(vert)) = (frag, vert) {
                            programs_attempted += 1;
                            if let Err(e) = link(gl, &vert, &frag) {
                                programs_failed += 1;
                                first_error = Some(format!("shaders/{}: {e}", pass.shader));
                                break 'mat;
                            }
                            programs_linked += 1;
                        }
                    }
                }
                if let Some(err) = first_error {
                    entry.1 = Some(err);
                }
            }
        }
    }

    let failed: Vec<(&String, &(usize, Option<String>))> =
        effects.iter().filter(|(_, (_, e))| e.is_some()).collect();

    println!("scenes scanned            : {}", dirs.len());
    println!("distinct effects          : {}", effects.len());
    println!("shader stages compiled    : {stages_ok}");
    println!("shader stages failed      : {stages_failed}");
    println!(
        "pass programs linked      : {programs_linked} of {programs_attempted} ({programs_failed} failed)"
    );
    println!(
        "effects with a broken stage: {} of {}",
        failed.len(),
        effects.len()
    );
    for (name, (uses, err)) in failed.iter().take(30) {
        println!("  {name} (used {uses}x)");
        println!("    {}", err.as_deref().unwrap_or("").trim());
    }
}

/// The driver's error lines, each followed by the source line it names.
fn error_lines(log: &str, assembled: &str) -> String {
    let src_lines: Vec<&str> = assembled.lines().collect();
    let mut detail = String::new();
    for l in log.lines() {
        let l = l.trim();
        if !l.contains("error") {
            continue;
        }
        detail.push_str(&format!("\n    {l}"));
        if let Some(rest) = l.strip_prefix("0(") {
            if let Some(close) = rest.find(')') {
                if let Ok(n) = rest[..close].trim().parse::<usize>() {
                    if let Some(src) = src_lines.get(n.saturating_sub(1)) {
                        detail.push_str(&format!("\n      > {}", src.trim()));
                    }
                }
            }
        }
        if detail.lines().count() >= 12 {
            break;
        }
    }
    detail
}

fn expand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    p.to_string()
}
