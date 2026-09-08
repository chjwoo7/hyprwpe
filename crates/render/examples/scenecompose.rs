//! Diagnostic: software-composite a scene.pkg's image layers to a PNG so the
//! transform/layout math can be validated without a GPU or a compositor.
//!
//! Usage: `cargo run -p hyprwpe-render --example scenecompose -- <scene.pkg> <out.png> [outW] [outH]`
use hyprwpe_core::pkg::Package;
use hyprwpe_core::scene::{ObjectKind, Scene};
use hyprwpe_core::tex::TexImage;
use image::{imageops, Rgba, RgbaImage};

fn is_image_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".tex")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".tga")
        || lower.ends_with(".bmp")
}

fn resolve_texture_entry(pkg: &Package, material_path: &str, name: &str) -> Option<String> {
    let _ = material_path;
    if is_image_file(name) {
        let rooted = format!("materials/{name}");
        let direct = name.to_string();
        return pkg
            .get(&rooted)
            .is_some()
            .then(|| rooted)
            .or_else(|| pkg.get(&direct).is_some().then(|| direct));
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
    let material = model.get("material")?.as_str()?;
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
    resolve_texture_entry(pkg, material, texture_name)
}

fn load_rgba(pkg: &Package, path: &str) -> Option<RgbaImage> {
    let raw = pkg.get(path)?;
    if path.ends_with(".tex") {
        let tex = TexImage::parse(raw).ok()?;
        return tex.to_rgba_image().ok();
    }
    image::load_from_memory(raw).ok().map(|i| i.to_rgba8())
}

struct Layer {
    img: RgbaImage,
    /// Quad centre in design units, half extents in design units, rotation deg.
    cx: f32,
    cy: f32,
    hw: f32,
    hh: f32,
    color: [f32; 4],
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pkg_path = &args[1];
    let out_path = &args[2];
    let out_w: u32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1920);
    let out_h: u32 = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(1080);

    let pkg = Package::open(std::path::Path::new(pkg_path)).expect("open pkg");
    let scene =
        Scene::from_json_str(pkg.get_str("scene.json").expect("scene.json")).expect("scene json");
    let raw_scene: serde_json::Value =
        serde_json::from_str(pkg.get_str("scene.json").unwrap()).unwrap();

    // Design canvas from general.orthogonalprojection, else the output size.
    let (mut dw, mut dh) = (out_w as f32, out_h as f32);
    if let Some(ortho) = raw_scene
        .get("general")
        .and_then(|g| g.get("orthogonalprojection"))
    {
        if let Some(w) = ortho.get("width").and_then(|v| v.as_f64()) {
            dw = w as f32;
        }
        if let Some(h) = ortho.get("height").and_then(|v| v.as_f64()) {
            dh = h as f32;
        }
    }
    // Also honour an explicit fov-free zoom in general.
    let zoom = raw_scene
        .get("general")
        .and_then(|g| g.get("zoom"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0) as f32;

    // Fit design canvas into output preserving aspect, honouring zoom.
    let fit = (out_w as f32 / dw).min(out_h as f32 / dh) * zoom;
    let ox = (out_w as f32 - dw * fit) / 2.0;
    let oy = (out_h as f32 - dh * fit) / 2.0;
    println!("design canvas {dw}x{dh} zoom {zoom} -> output {out_w}x{out_h}, fit scale {fit}");

    let mut layers: Vec<Layer> = Vec::new();
    for obj in &scene.objects {
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

        // Base world size: the texture's own size when the object does not
        // declare one (model `autosize`), else the declared `size`.
        let (mut sw, mut sh) = (tw as f32, th as f32);
        if let Some(s) = obj.size() {
            if s[0] > 0.0 && s[1] > 0.0 {
                sw = s[0];
                sh = s[1];
            }
        }
        let sc = obj.scale();
        let origin = obj.origin();
        let angles = obj.angles();

        let color_rgb = obj.color.map(|c| [c.x(), c.y(), c.z()]).unwrap_or([1.0, 1.0, 1.0]);
        let alpha = obj.alpha();

        // WE places the quad centred on `origin`; `size` is the quad's extent
        // before `scale`. The rotation is around the centre.
        layers.push(Layer {
            img,
            cx: origin[0],
            cy: origin[1],
            hw: sw * sc[0] / 2.0,
            hh: sh * sc[1] / 2.0,
            color: [color_rgb[0], color_rgb[1], color_rgb[2], alpha],
        });
        let _ = angles;
    }
    println!("compositing {} layers", layers.len());

    let mut canvas = RgbaImage::new(out_w, out_h);
    let black = Rgba([0, 0, 0, 255]);
    imageops::replace(
        &mut canvas,
        &RgbaImage::from_pixel(out_w, out_h, black),
        0,
        0,
    );

    for l in &layers {
        let quad_w = (l.hw * 2.0 * fit).round().max(1.0) as u32;
        let quad_h = (l.hh * 2.0 * fit).round().max(1.0) as u32;
        if quad_w > 32768 || quad_h > 32768 {
            continue;
        }
        let center_px = (ox + l.cx * fit, oy + l.cy * fit);
        let x0 = (center_px.0 - quad_w as f32 / 2.0).round() as i64;
        let y0 = (center_px.1 - quad_h as f32 / 2.0).round() as i64;
        if (x0 + quad_w as i64) < 0
            || (y0 + quad_h as i64) < 0
            || x0 > out_w as i64
            || y0 > out_h as i64
        {
            continue;
        }

        let quad = imageops::resize(
            &l.img,
            quad_w,
            quad_h,
            imageops::FilterType::Triangle,
        );

        for yy in 0..quad.height() {
            for xx in 0..quad.width() {
                let px = x0 + xx as i64;
                let py = y0 + yy as i64;
                if px < 0 || py < 0 || px >= out_w as i64 || py >= out_h as i64 {
                    continue;
                }
                let s = *quad.get_pixel(xx, yy);
                let a = s.0[3] as f32 / 255.0 * l.color[3];
                if a <= 0.0 {
                    continue;
                }
                let d = canvas.get_pixel_mut(px as u32, py as u32);
                let inv = 1.0 - a;
                d.0[0] = ((s.0[0] as f32 * l.color[0]) * a + d.0[0] as f32 * inv) as u8;
                d.0[1] = ((s.0[1] as f32 * l.color[1]) * a + d.0[1] as f32 * inv) as u8;
                d.0[2] = ((s.0[2] as f32 * l.color[2]) * a + d.0[2] as f32 * inv) as u8;
                d.0[3] = 255;
            }
        }
    }
    canvas.save(out_path).expect("save");
    println!("wrote {out_path}");
}
