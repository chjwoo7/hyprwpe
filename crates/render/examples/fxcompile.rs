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
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `EGL_PLATFORM_SURFACELESS_MESA`. khronos-egl types `Enum` as `c_uint`, so the
/// constant is passed directly rather than looked up.
const EGL_PLATFORM_SURFACELESS_MESA: khronos_egl::Enum = 0x31DD;

/// The renderer loads EGL 1.4, which has no platform-display entry point. The
/// surfaceless platform comes from `EGL_EXT_platform_base`, so this harness
/// resolves `eglGetPlatformDisplayEXT` itself - the core 1.5
/// `eglGetPlatformDisplay` is not exported by every driver's `libEGL` (it
/// answers `EGL_BAD_PARAMETER` on glvnd here, while the `EXT` entry point
/// works), which is exactly the kind of difference this harness exists to
/// surface.
type Egl = khronos_egl::DynamicInstance<khronos_egl::EGL1_4>;

/// `eglGetPlatformDisplayEXT`
type GetPlatformDisplayExt =
    unsafe extern "system" fn(u32, *mut std::ffi::c_void, *const i32) -> *mut std::ffi::c_void;

struct Gl {
    /// Kept for the teardown in `Drop`; the instance itself is shared.
    egl: std::sync::Arc<Egl>,
    display: khronos_egl::Display,
    context: khronos_egl::Context,
    gl: glow::Context,
}

impl Gl {
    /// A GLES 3 context with no window attached.
    ///
    /// Mesa advertises `EGL_KHR_surfaceless_context`, so `make_current` succeeds
    /// with `NO_SURFACE` and everything drawn goes to framebuffers. If a driver
    /// lacks it, a 1x1 pbuffer is created instead so the harness still runs.
    fn surfaceless() -> anyhow::Result<Self> {
        use khronos_egl as egl;
        let egl = unsafe { Egl::load_required() }
            .map_err(|e| anyhow::anyhow!("loading libEGL: {e:?}"))?;

        let proc = egl
            .get_proc_address("eglGetPlatformDisplayEXT")
            .ok_or_else(|| anyhow::anyhow!("this libEGL has no eglGetPlatformDisplayEXT"))?;
        let get_platform_display: GetPlatformDisplayExt = unsafe { std::mem::transmute(proc) };
        let raw = unsafe {
            get_platform_display(
                EGL_PLATFORM_SURFACELESS_MESA,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if raw.is_null() {
            let code = egl.get_error().map(|e| e as u32).unwrap_or(0);
            anyhow::bail!("no surfaceless EGL display (EGL error {code:#x})");
        }
        let display = unsafe { egl::Display::from_ptr(raw) };
        egl.initialize(display)
            .map_err(|e| anyhow::anyhow!("initialising EGL: {e:?}"))?;
        egl.bind_api(egl::OPENGL_ES_API)
            .map_err(|e| anyhow::anyhow!("binding ES API: {e:?}"))?;

        let config_attributes = [
            egl::SURFACE_TYPE,
            egl::PBUFFER_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES3_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &config_attributes)
            .map_err(|e| anyhow::anyhow!("choosing a config: {e:?}"))?
            .ok_or_else(|| anyhow::anyhow!("no GLES 3 config available"))?;

        let context_attributes = [egl::CONTEXT_MAJOR_VERSION, 3, egl::NONE];
        let context = egl
            .create_context(display, config, None, &context_attributes)
            .map_err(|e| anyhow::anyhow!("creating a GLES 3 context: {e:?}"))?;

        // Surfaceless first; a pbuffer is the fallback for drivers without it.
        if egl
            .make_current(display, None, None, Some(context))
            .is_err()
        {
            let pbuffer = egl
                .create_pbuffer_surface(
                    display,
                    config,
                    &[egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE],
                )
                .map_err(|e| anyhow::anyhow!("no surfaceless context and no pbuffer: {e:?}"))?;
            egl.make_current(display, Some(pbuffer), Some(pbuffer), Some(context))
                .map_err(|e| anyhow::anyhow!("making the pbuffer current: {e:?}"))?;
        }

        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name)
                    .map(|p| p as *const std::ffi::c_void)
                    .unwrap_or(std::ptr::null())
            })
        };
        Ok(Gl {
            egl: egl.into(),
            display,
            context,
            gl,
        })
    }

    fn version(&self) -> String {
        unsafe {
            format!(
                "{} | {}",
                self.gl.get_parameter_string(glow::VERSION),
                self.gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION)
            )
        }
    }

    /// Compile one stage, returning the driver's log on failure.
    fn compile(&self, kind: u32, source: &str) -> Result<(), String> {
        unsafe {
            let shader = self
                .gl
                .create_shader(kind)
                .map_err(|e| format!("create_shader: {e}"))?;
            self.gl.shader_source(shader, source);
            self.gl.compile_shader(shader);
            let ok = self.gl.get_shader_compile_status(shader);
            let log = self.gl.get_shader_info_log(shader);
            self.gl.delete_shader(shader);
            if ok {
                Ok(())
            } else {
                Err(log)
            }
        }
    }
}

impl Drop for Gl {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("HYPRWPE_WORKSHOP").ok())
        .unwrap_or_else(|| "~/.steam/root/steamapps/workshop/content/431960".into());
    let root = PathBuf::from(expand(&root));

    let gl = match Gl::surfaceless() {
        Ok(gl) => gl,
        Err(e) => {
            eprintln!("no headless GL context: {e:#}");
            eprintln!("(this harness needs EGL with the surfaceless platform)");
            std::process::exit(2);
        }
    };
    println!("GL context: {}", gl.version());

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read workshop dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").is_file())
        .collect();
    dirs.sort();

    let mut effects: BTreeMap<String, (usize, Option<String>)> = BTreeMap::new();
    let mut stages_ok = 0usize;
    let mut stages_failed = 0usize;

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
                                    first_error = Some(format!("{path}: {e}"));
                                    break 'mat;
                                }
                            };
                            let assembled = effect::assemble(&expanded, stage, &[]);
                            let kind = if stage == Stage::Fragment {
                                glow::FRAGMENT_SHADER
                            } else {
                                glow::VERTEX_SHADER
                            };
                            match gl.compile(kind, &assembled) {
                                Ok(()) => stages_ok += 1,
                                Err(log) => {
                                    stages_failed += 1;
                                    if std::env::var_os("FXDUMP").is_some() {
                                        let base = path.replace('/', "_");
                                        let out = format!("/tmp/fx_{base}.glsl");
                                        let _ = std::fs::write(&out, &assembled);
                                        eprintln!("dumped {out}");
                                    }
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
                                                if let Ok(n) = rest[..close].trim().parse::<usize>()
                                                {
                                                    if let Some(src) =
                                                        src_lines.get(n.saturating_sub(1))
                                                    {
                                                        detail.push_str(&format!(
                                                            "\n      > {}",
                                                            src.trim()
                                                        ));
                                                    }
                                                }
                                            }
                                        }
                                        if detail.lines().count() >= 12 {
                                            break;
                                        }
                                    }
                                    first_error = Some(format!("{path}:{detail}"));
                                    break 'mat;
                                }
                            }
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
        "effects with a broken stage: {} of {}",
        failed.len(),
        effects.len()
    );
    for (name, (uses, err)) in failed.iter().take(30) {
        println!("  {name} (used {uses}x)");
        println!("    {}", err.as_deref().unwrap_or("").trim());
    }
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
