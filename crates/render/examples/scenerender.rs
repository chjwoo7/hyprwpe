//! Render a real scene wallpaper headlessly and report what came out.
//!
//! This is the scene renderer's own end-to-end check, run on the surfaceless EGL
//! context so nothing reaches a monitor. It renders a `scene.pkg` through
//! `ScenePlayer` - the same code path the daemon uses - into an off-screen
//! framebuffer, then measures the result: how much of the frame is not the clear
//! colour, and what the mean colour is.
//!
//! That makes it the fastest honest answer to "does this wallpaper render?", and
//! the way an effect change is proven to reach pixels without a live session:
//!
//! ```text
//! scenerender <scene.pkg> out.png 1920 1080 fill [time] [--no-effects]
//! ```
//!
//! `--no-effects` skips the effect chains, so the two runs differ by exactly the
//! effect contribution and nothing else.

use glow::HasContext;
use hyprwpe_core::assets::Resources;
use hyprwpe_render::headless::Headless;
use hyprwpe_render::scene_layer::ScenePlayer;
use hyprwpe_render::Scaling;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(pkg_path) = args.get(1) else {
        eprintln!("usage: scenerender <scene.pkg> [out.png] [W H [mode]] [--no-effects]");
        std::process::exit(2);
    };
    let out_path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "/tmp/scenerender.png".into());
    let width: i32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1920);
    let height: i32 = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(1080);
    let scaling = args
        .get(5)
        .and_then(|a| Scaling::parse(a))
        .unwrap_or(Scaling::Fill);
    let effects = !args.iter().any(|a| a == "--no-effects");
    let json_out = args.iter().any(|a| a == "--json");
    let particles = !args.iter().any(|a| a == "--no-particles");
    // A scene at t=0 is not representative: particles have not spawned yet and
    // animation is at its first keyframe, so a corpus measured at zero would
    // report a wallpaper as blank when it is simply waiting to start.
    let at_time: f32 = args
        .iter()
        .position(|a| a == "--time")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);

    // `--set key=value` applies a wallpaper setting, so a property change can be
    // followed all the way to pixels: property -> effect uniform -> framebuffer.
    let mut overrides: Vec<(String, String)> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--set" {
            if let Some(kv) = args.get(i + 1) {
                if let Some((k, v)) = kv.split_once('=') {
                    overrides.push((k.to_string(), v.to_string()));
                }
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    let mut properties =
        hyprwpe_core::properties::PropertySet::for_wallpaper(std::path::Path::new(pkg_path));
    for (key, raw) in &overrides {
        match properties.get(key) {
            Some(prop) => {
                let value = serde_json::from_str(raw)
                    .unwrap_or_else(|_| serde_json::Value::String(raw.clone()));
                match prop.coerce(&value) {
                    Some(v) => {
                        let coerced = v;
                        if let Some(p) = properties.properties.iter_mut().find(|p| p.key == *key) {
                            p.value = coerced;
                        }
                        println!("set {key} = {value}");
                    }
                    None => eprintln!("warning: {raw} is not a valid value for {key}"),
                }
            }
            None => {
                let known: Vec<&str> = properties.editable().map(|p| p.key.as_str()).collect();
                eprintln!(
                    "warning: no property {key:?} on this wallpaper; it has: {}",
                    known.join(", ")
                );
            }
        }
    }

    let ctx = match Headless::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no headless GL context: {e:#}");
            std::process::exit(2);
        }
    };
    let gl = &ctx.gl;
    println!("GL context: {}", ctx.version());

    // An off-screen framebuffer the size of the output we want to inspect.
    let (fbo, texture) = unsafe {
        let texture = gl.create_texture().expect("create texture");
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
        let fbo = gl.create_framebuffer().expect("create framebuffer");
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            eprintln!("framebuffer incomplete ({status:#x})");
            std::process::exit(1);
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        (fbo, texture)
    };

    let mut player = match ScenePlayer::with_properties(
        std::path::Path::new(pkg_path),
        gl,
        scaling,
        &properties,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("loading {pkg_path}: {e:#}");
            std::process::exit(1);
        }
    };
    player.set_effects_enabled(effects);
    player.set_particles_enabled(particles);
    player.advance_to(at_time);
    let info = player.describe();
    if !json_out {
        println!("scene: {info}");
    }

    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
    }
    if let Err(e) = player.render(gl, width, height) {
        eprintln!("render: {e:#}");
        std::process::exit(1);
    }
    let pixels = ctx.read_framebuffer(fbo, width, height);

    let stats = Stats::of(&pixels, width, height, player.clear_color());
    if json_out {
        // One object per wallpaper, so a corpus run is a data file rather than
        // prose nobody can diff.
        let obj = serde_json::json!({
            "scene": info,
            "covered_pct": (stats.not_clear * 10000.0).round() / 100.0,
            "mean_rgb": stats.mean,
            "mean_alpha": stats.mean_alpha,
            "clear": player.clear_color(),
        });
        println!("{obj}");
    } else {
        println!("{stats}");
    }

    if let Some(img) = image::RgbaImage::from_raw(width as u32, height as u32, pixels) {
        // Sixteen bits is not enough for a 4K frame; fall back to RGBA8.
        if let Err(e) = img.save(&out_path) {
            eprintln!("writing {out_path}: {e}");
        } else {
            println!("wrote {out_path}");
        }
    }

    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(fbo);
        gl.delete_texture(texture);
    }
    // Keep the resources check meaningful: a package with no engine assets loses
    // particle sprites, and saying so is better than a silent gap.
    if let Ok(res) = Resources::open(std::path::Path::new(pkg_path)) {
        if !res.has_assets() {
            println!(
                "note: no Wallpaper Engine assets found; built-in sprites/effects will be missing"
            );
        }
    }
}

struct Stats {
    not_clear: f64,
    mean: [f32; 3],
    mean_alpha: f32,
}

impl Stats {
    fn of(pixels: &[u8], width: i32, height: i32, clear: [f32; 4]) -> Self {
        let n = (width as usize * height as usize).max(1);
        // The clear colour in 8-bit, which is what an uncovered pixel holds.
        let target: [u8; 3] = [
            (clear[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (clear[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (clear[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
        let mut not_clear = 0usize;
        let mut sum = [0f64; 3];
        let mut alpha = 0f64;
        for px in pixels.as_chunks::<4>().0 {
            // A couple of levels of tolerance: a clear colour that went through
            // an sRGB-ish blend can land a unit or two off.
            let differs = (0..3).any(|c| px[c].abs_diff(target[c]) > 2);
            if differs {
                not_clear += 1;
            }
            for c in 0..3 {
                sum[c] += px[c] as f64;
            }
            alpha += px[3] as f64;
        }
        Stats {
            not_clear: not_clear as f64 / n as f64,
            mean: [
                (sum[0] / n as f64) as f32,
                (sum[1] / n as f64) as f32,
                (sum[2] / n as f64) as f32,
            ],
            mean_alpha: (alpha / n as f64) as f32,
        }
    }
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "covered {:.2}% of the frame (clear colour elsewhere), mean rgb [{:.1}, {:.1}, {:.1}], mean alpha {:.1}",
            self.not_clear * 100.0,
            self.mean[0],
            self.mean[1],
            self.mean[2],
            self.mean_alpha
        )
    }
}
