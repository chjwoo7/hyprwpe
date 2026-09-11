//! Diagnostic: software-composite a `scene.pkg`'s image layers to a PNG so the
//! transform and layout math can be checked without a GPU or a compositor.
//!
//! It drives the *same* [`hyprwpe_render::scene_transform`] module the GL
//! renderer drives — parent composition, the design-canvas map, the clear
//! colour — so a framing bug shows up here exactly as it does on screen. Only
//! the rasteriser differs (inverse-mapped sampling instead of a GPU).
//!
//! Usage: `cargo run -p hyprwpe-render --example scenecompose -- \
//!         <scene.pkg> <out.png> [width] [height] [fill|fit|stretch|center]`

use hyprwpe_core::pkg::Package;
use hyprwpe_core::scene::{ObjectKind, Scene};
use hyprwpe_core::tex::TexImage;
use hyprwpe_render::scaling::Scaling;
use hyprwpe_render::scene_transform::{canvas_map, clear_color, world_transforms, Affine};
use image::{Rgba, RgbaImage};

fn is_image_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".tex")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".tga")
        || lower.ends_with(".bmp")
}

fn resolve_texture_entry(pkg: &Package, name: &str) -> Option<String> {
    if is_image_file(name) {
        let rooted = format!("materials/{name}");
        return pkg
            .get(&rooted)
            .is_some()
            .then_some(rooted)
            .or_else(|| pkg.get(name).is_some().then_some(name.to_string()));
    }
    for ext in [".tex", ".png", ".jpg", ".jpeg", ".tga", ".bmp", ""] {
        let cand = format!("materials/{name}{ext}");
        if pkg.get(&cand).is_some() {
            return Some(cand);
        }
    }
    None
}

fn resolve_texture_file(pkg: &Package, image_ref: &str) -> Option<String> {
    if is_image_file(image_ref) {
        return pkg.get(image_ref).is_some().then(|| image_ref.to_string());
    }
    let model = pkg.get_str(image_ref)?;
    let model: serde_json::Value = serde_json::from_str(model).ok()?;
    match model.get("material").and_then(|m| m.as_str()) {
        Some(material) => {
            if is_image_file(material) {
                return pkg.get(material).is_some().then(|| material.to_string());
            }
            let mat = pkg.get_str(material)?;
            let mat: serde_json::Value = serde_json::from_str(mat).ok()?;
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
            resolve_texture_entry(pkg, texture_name)
        }
        None => {
            let texture_name = model
                .get("passes")
                .and_then(|p| p.as_array())
                .and_then(|p| p.first())
                .and_then(|p0| p0.get("textures"))
                .and_then(|t| t.as_array())
                .and_then(|t| t.first())
                .and_then(|v| v.as_str())
                .or_else(|| {
                    model
                        .get("textures")
                        .and_then(|t| t.as_array())
                        .and_then(|t| t.first())
                        .and_then(|v| v.as_str())
                })?;
            resolve_texture_entry(pkg, texture_name)
        }
    }
}

fn load_rgba(pkg: &Package, path: &str) -> Option<RgbaImage> {
    let raw = pkg.get(path)?;
    let img = if path.ends_with(".tex") {
        let tex = TexImage::parse(raw).ok()?;
        tex.to_rgba_image().ok()?
    } else {
        image::load_from_memory(raw).ok()?.to_rgba8()
    };
    // Mirror scene_layer's guard: refuse degenerate decodes that would paint
    // stretched scanline noise instead of a layer.
    let (w, h) = img.dimensions();
    if w <= 1 || h <= 1 || w > 16384 || h > 16384 {
        return None;
    }
    Some(img)
}

struct Layer {
    img: RgbaImage,
    /// Quad coordinates (-1..1) -> design units, ancestors composed in.
    to_design: Affine,
    color: [f32; 4],
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pkg_path = &args[1];
    let out_path = &args[2];
    let out_w: u32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1920);
    let out_h: u32 = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(1080);
    let scaling = args
        .get(5)
        .and_then(|a| Scaling::parse(a))
        .unwrap_or(Scaling::Fill);

    let pkg = Package::open(std::path::Path::new(pkg_path)).expect("open pkg");
    let scene_json = pkg.get_str("scene.json").expect("scene.json");
    let scene = Scene::from_json_str(scene_json).expect("scene json");
    let raw: serde_json::Value = serde_json::from_str(scene_json).unwrap();

    let design = raw
        .get("general")
        .and_then(|g| g.get("orthogonalprojection"))
        .and_then(|o| {
            Some((
                o.get("width")?.as_f64()? as f32,
                o.get("height")?.as_f64()? as f32,
            ))
        })
        .filter(|(w, h)| *w > 0.0 && *h > 0.0);
    let zoom = raw
        .get("general")
        .and_then(|g| g.get("zoom"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0) as f32;

    let map = canvas_map(design, zoom, out_w as f32, out_h as f32, scaling);
    println!("design {design:?} zoom {zoom} scaling {scaling:?} -> map {map:?}");

    let world = world_transforms(&scene.objects);
    let mut layers: Vec<Layer> = Vec::new();
    for (i, obj) in scene.objects.iter().enumerate() {
        if obj.kind() != ObjectKind::Image || !obj.is_visible() {
            continue;
        }
        let Some(image_ref) = obj.image_path() else {
            continue;
        };
        let Some(entry) = resolve_texture_file(&pkg, &image_ref) else {
            eprintln!("  unresolved: {image_ref}");
            continue;
        };
        let Some(img) = load_rgba(&pkg, &entry) else {
            eprintln!("  undecodable: {entry}");
            continue;
        };
        let (tw, th) = img.dimensions();

        // `size` is the quad extent before `scale`; a model without one draws
        // at the texture's own size.
        let (mut sw, mut sh) = (tw as f32, th as f32);
        if let Some(s) = obj.size() {
            if s[0] > 0.0 && s[1] > 0.0 {
                sw = s[0];
                sh = s[1];
            }
        }
        let color_rgb = obj
            .color
            .map(|c| [c.x(), c.y(), c.z()])
            .unwrap_or([1.0, 1.0, 1.0]);
        let alpha = obj.alpha();

        layers.push(Layer {
            img,
            to_design: world[i].scale_linear(sw / 2.0, sh / 2.0),
            color: [color_rgb[0], color_rgb[1], color_rgb[2], alpha],
        });
    }
    println!("compositing {} layers", layers.len());

    let clear = clear_color(&scene);
    let bg = Rgba([
        (clear[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (clear[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (clear[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (clear[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]);
    let mut canvas = RgbaImage::from_pixel(out_w, out_h, bg);

    let w_f = out_w as f32;
    let h_f = out_h as f32;
    for l in &layers {
        let Some(inv) = l.to_design.invert() else {
            continue;
        };
        // Output-pixel space of the design canvas: x right, y down.
        let to_px = |dx: f32, dy: f32| -> (f32, f32) {
            (map.x + dx * map.sx, h_f - (map.y + dy * map.sy))
        };
        let corners = [
            l.to_design.apply(-1.0, -1.0),
            l.to_design.apply(1.0, -1.0),
            l.to_design.apply(-1.0, 1.0),
            l.to_design.apply(1.0, 1.0),
        ];
        let mut minx = f32::INFINITY;
        let mut maxx = f32::NEG_INFINITY;
        let mut miny = f32::INFINITY;
        let mut maxy = f32::NEG_INFINITY;
        for (dx, dy) in corners {
            let (px, py) = to_px(dx, dy);
            minx = minx.min(px);
            maxx = maxx.max(px);
            miny = miny.min(py);
            maxy = maxy.max(py);
        }
        let x0 = minx.floor().max(0.0) as i64;
        let x1 = maxx.ceil().min(w_f) as i64;
        let y0 = miny.floor().max(0.0) as i64;
        let y1 = maxy.ceil().min(h_f) as i64;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }

        let (twf, thf) = (l.img.width() as f32, l.img.height() as f32);
        for py in y0..y1 {
            for px in x0..x1 {
                // Output pixel -> design units.
                let dx = (px as f32 + 0.5 - map.x) / map.sx;
                let dy = ((h_f - (py as f32 + 0.5)) - map.y) / map.sy;
                let (u, v) = inv.apply(dx, dy);
                if !(-1.0..=1.0).contains(&u) || !(-1.0..=1.0).contains(&v) {
                    continue;
                }
                // Quad space -> texture UV, matching the GL quad's winding
                // (v = +1 is the texture's top row).
                let su = (u + 1.0) * 0.5 * (twf - 1.0);
                let sv = (1.0 - v) * 0.5 * (thf - 1.0);
                let s = sample(&l.img, su, sv);
                let a = s[3] as f32 / 255.0 * l.color[3];
                if a <= 0.0 {
                    continue;
                }
                let d = canvas.get_pixel_mut(px as u32, py as u32);
                let inv_a = 1.0 - a;
                for (c, sc) in s.iter().take(3).enumerate() {
                    d.0[c] = ((*sc as f32 * l.color[c]) * a + d.0[c] as f32 * inv_a)
                        .clamp(0.0, 255.0) as u8;
                }
                d.0[3] = 255;
            }
        }
    }

    canvas.save(out_path).expect("save");
    println!("wrote {out_path}");
}

/// Bilinear sample with edge clamping.
fn sample(img: &RgbaImage, x: f32, y: f32) -> [u8; 4] {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if w <= 0 || h <= 0 {
        return [0, 0, 0, 0];
    }
    let fx = x.floor();
    let fy = y.floor();
    let tx = x - fx;
    let ty = y - fy;
    let clamp = |v: i64, hi: i64| v.clamp(0, hi.max(0));
    let p = |ix: i64, iy: i64| -> [f32; 4] {
        let px = img.get_pixel(clamp(ix, w - 1) as u32, clamp(iy, h - 1) as u32).0;
        [
            px[0] as f32,
            px[1] as f32,
            px[2] as f32,
            px[3] as f32,
        ]
    };
    let (x0, y0) = (fx as i64, fy as i64);
    let c00 = p(x0, y0);
    let c10 = p(x0 + 1, y0);
    let c01 = p(x0, y0 + 1);
    let c11 = p(x0 + 1, y0 + 1);
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = c00[c] * (1.0 - tx) + c10[c] * tx;
        let bot = c01[c] * (1.0 - tx) + c11[c] * tx;
        out[c] = (top * (1.0 - ty) + bot * ty).clamp(0.0, 255.0) as u8;
    }
    out
}
