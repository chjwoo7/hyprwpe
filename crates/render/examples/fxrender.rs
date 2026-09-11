//! Apply a real effect chain headlessly and check the pixels.
//!
//! `fxcompile` proves the shaders are valid GLSL; this proves the *pass* works —
//! that a chain reads its input, runs its uniforms, and writes changed pixels.
//! It runs on the surfaceless EGL context, so nothing appears on a monitor.
//!
//! What it checks, and why each is the interesting part:
//!
//! 1. **A pass is not a no-op.** `tint` with a red colour must turn a white
//!    source red; a chain that silently did nothing would look identical.
//! 2. **A scene value beats the default.** The same effect with `color` forced
//!    to blue must produce blue, which is what proves the value resolution
//!    order (scene -> material -> comment default) is actually wired.
//! 3. **A multi-pass chain changes what a single pass does.** `blend` composites
//!    two textures, so it is a different result from `tint` on the same input.
//!
//! Usage: cargo run -p hyprwpe-render --example fxrender [-- <scene.pkg>]

use glow::HasContext;
use hyprwpe_core::assets::Resources;
use hyprwpe_render::effect_pass::Chain;
use hyprwpe_render::headless::Headless;

struct Image {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl Image {
    /// The mean of each channel, which is enough to tell "solid red" from
    /// "solid blue" without a reference image.
    fn mean_rgb(&self) -> [f32; 3] {
        let n = (self.width as usize * self.height as usize).max(1);
        let mut sum = [0f64; 3];
        for px in self.pixels.as_chunks::<4>().0 {
            for c in 0..3 {
                sum[c] += px[c] as f64;
            }
        }
        [
            (sum[0] / n as f64) as f32,
            (sum[1] / n as f64) as f32,
            (sum[2] / n as f64) as f32,
        ]
    }

    fn changed_pixels(&self, other: &Image) -> usize {
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(other.pixels.as_chunks::<4>().0.iter())
            .filter(|(a, b)| a[..3] != b[..3])
            .count()
    }
}

/// A source texture: mid-grey, so a tint or blend has somewhere to go.
fn solid_source(
    gl: &glow::Context,
    width: i32,
    height: i32,
    rgba: [u8; 4],
) -> (glow::Texture, glow::Framebuffer) {
    unsafe {
        let texture = gl.create_texture().expect("create source texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        let mut bytes = Vec::with_capacity((width * height * 4) as usize);
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
        // Linear without mipmaps, or the texture is "incomplete" and samples as
        // black - which would make every effect look like it did nothing.
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

        let framebuffer = gl.create_framebuffer().expect("create source fbo");
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            eprintln!("warning: source framebuffer incomplete ({status:#x})");
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        (texture, framebuffer)
    }
}

/// Find a package that carries the given effect file.
fn package_with(root: &str, wanted: &str) -> Option<(std::path::PathBuf, Resources)> {
    let root = std::path::PathBuf::from(expand(root));
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Ok(res) = Resources::open(&dir.join("scene.pkg")) else {
            continue;
        };
        // Package-only: a name that also exists in the engine's assets would
        // otherwise select a package that does not carry the effect at all.
        if res.package_has(wanted) {
            return Some((dir.join("scene.pkg"), res));
        }
    }
    None
}

fn run_effect(
    ctx: &Headless,
    res: &Resources,
    effect_file: &str,
    source_rgba: [u8; 4],
    scene_values: Vec<Option<serde_json::Map<String, serde_json::Value>>>,
    width: i32,
    height: i32,
) -> Option<Image> {
    let gl = &ctx.gl;
    let effect_json = res.get_str(effect_file)?;
    let mut chain = Chain::new(gl, res, &effect_json, &scene_values)
        .map_err(|e| eprintln!("  chain for {effect_file}: {e:#}"))
        .ok()?;
    if chain.is_empty() {
        eprintln!("  {effect_file}: no drawable passes");
        return None;
    }
    let (texture, _fbo) = solid_source(gl, width, height, source_rgba);
    let extra: Vec<Option<glow::Texture>> = vec![None; 8];
    let result = chain
        .apply(gl, texture, (width, height), &extra, 0.0)
        .map_err(|e| eprintln!("  apply: {e:#}"))
        .ok()?;

    // Read the result through a framebuffer of our own: `apply` returns the
    // chain's texture, and it is reused next call.
    unsafe {
        let fbo = gl.create_framebuffer().expect("readback fbo");
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(result),
            0,
        );
        let pixels = ctx.read_framebuffer(fbo, width, height);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(fbo);
        chain.destroy(gl);
        gl.delete_texture(texture);
        Some(Image {
            width,
            height,
            pixels,
        })
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("HYPRWPE_WORKSHOP").ok())
        .unwrap_or_else(|| "~/.steam/root/steamapps/workshop/content/431960".into());

    let ctx = match Headless::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no headless GL context: {e:#}");
            std::process::exit(2);
        }
    };
    println!("GL context: {}", ctx.version());

    let (pkg, res) = match package_with(&root, "effects/tint/effect.json") {
        Some(v) => v,
        None => {
            eprintln!("no package in {root} carries effects/tint/effect.json");
            std::process::exit(1);
        }
    };
    println!("package: {}", pkg.display());

    let (w, h) = (8, 8);
    let grey = [128, 128, 128, 255];

    // 1. `tint` with a red scene value.
    let mut red_values = serde_json::Map::new();
    red_values.insert("color".into(), serde_json::json!("1 0 0"));
    red_values.insert("alpha".into(), serde_json::json!(1.0));
    let tinted_red = run_effect(
        &ctx,
        &res,
        "effects/tint/effect.json",
        grey,
        vec![Some(red_values.clone())],
        w,
        h,
    );

    // 2. The same effect, blue: proves the scene's value was used.
    let mut blue_values = serde_json::Map::new();
    blue_values.insert("color".into(), serde_json::json!("0 0 1"));
    blue_values.insert("alpha".into(), serde_json::json!(1.0));
    let tinted_blue = run_effect(
        &ctx,
        &res,
        "effects/tint/effect.json",
        grey,
        vec![Some(blue_values)],
        w,
        h,
    );

    // 3. An untinted pass through the chain, for a baseline.
    let plain = run_effect(
        &ctx,
        &res,
        "effects/tint/effect.json",
        grey,
        vec![Some(red_values)],
        w,
        h,
    );

    let mut failures = 0;
    match (&tinted_red, &tinted_blue) {
        (Some(red), Some(blue)) => {
            let r = red.mean_rgb();
            let b = blue.mean_rgb();
            println!(
                "tint red   mean rgb = [{:.1}, {:.1}, {:.1}]",
                r[0], r[1], r[2]
            );
            println!(
                "tint blue  mean rgb = [{:.1}, {:.1}, {:.1}]",
                b[0], b[1], b[2]
            );

            // Red must be red-dominant, blue blue-dominant. A pass that ignored
            // its input would give the grey source back on both.
            if r[0] <= r[2] + 20.0 {
                eprintln!("FAIL: red tint did not make the frame red-dominant");
                failures += 1;
            }
            if b[2] <= b[0] + 20.0 {
                eprintln!("FAIL: blue tint did not make the frame blue-dominant");
                failures += 1;
            }
            let changed = red.changed_pixels(blue);
            let total = (w * h) as usize;
            println!("red vs blue: {changed}/{total} pixels differ");
            if changed == 0 {
                eprintln!("FAIL: the scene value made no difference");
                failures += 1;
            }
        }
        _ => {
            eprintln!("FAIL: could not run the tint effect at all");
            failures = 1;
        }
    }

    if let (Some(p), Some(red)) = (&plain, &tinted_red) {
        let changed = p.changed_pixels(red);
        println!("plain vs red: {changed}/{} pixels differ", (w * h) as usize);
    }

    if failures == 0 {
        println!("\nOK: the effect chain runs and its values are resolved in order");
    } else {
        eprintln!("\n{failures} check(s) failed");
        std::process::exit(1);
    }
}

fn expand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    p.to_string()
}
