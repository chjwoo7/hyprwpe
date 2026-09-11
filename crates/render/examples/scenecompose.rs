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

use hyprwpe_core::assets::Resources;
use hyprwpe_core::particle as particle_def;
use hyprwpe_core::scene::{ObjectKind, Scene};
use hyprwpe_core::tex::TexImage;
use hyprwpe_render::particle::Sim as ParticleSim;
use hyprwpe_render::scaling::Scaling;
use hyprwpe_render::scene_layer::resolve_puppet_file;
use hyprwpe_render::scene_transform::{
    animated_alpha, animated_world_transforms, canvas_map, clear_color, particle_sprite_transform,
    puppet_placement, Affine,
};
use hyprwpe_render::skin::{deform, Rig};
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

fn resolve_texture_entry(pkg: &Resources, name: &str) -> Option<String> {
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

fn resolve_texture_file(pkg: &Resources, image_ref: &str) -> Option<String> {
    if is_image_file(image_ref) {
        return pkg.get(image_ref).is_some().then(|| image_ref.to_string());
    }
    let model = pkg.get_str(image_ref)?;
    let model: serde_json::Value = serde_json::from_str(&model).ok()?;
    match model.get("material").and_then(|m| m.as_str()) {
        Some(material) => {
            if is_image_file(material) {
                return pkg.get(material).is_some().then(|| material.to_string());
            }
            let mat = pkg.get_str(material)?;
            let mat: serde_json::Value = serde_json::from_str(&mat).ok()?;
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

fn load_rgba(pkg: &Resources, path: &str) -> Option<RgbaImage> {
    let (raw, sidecar) = pkg.texture(path)?;
    let img = if path.ends_with(".tex") {
        let tex = TexImage::parse_with_sidecar(&raw, sidecar.as_deref()).ok()?;
        tex.to_rgba_image().ok()?
    } else {
        image::load_from_memory(&raw).ok()?.to_rgba8()
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

/// A running particle system, ready to rasterise at a requested time.
struct ParticleSys {
    sim: ParticleSim,
    img: RgbaImage,
    placement: Affine,
    color: [f32; 4],
}

/// Draw a textured `-1..1` quad through `to_design`, inverse-mapped the same way
/// the GL renderer samples it. Shared by image layers and particle sprites so a
/// sprite cannot be placed differently from a layer.
#[allow(clippy::too_many_arguments)]
fn raster_quad(
    canvas: &mut RgbaImage,
    map: &hyprwpe_render::scene_transform::CanvasMap,
    img: &RgbaImage,
    to_design: &Affine,
    color: [f32; 4],
    w_f: f32,
    h_f: f32,
) {
    let Some(inv) = to_design.invert() else {
        return;
    };
    let to_px =
        |dx: f32, dy: f32| -> (f32, f32) { (map.x + dx * map.sx, h_f - (map.y + dy * map.sy)) };
    let corners = [
        to_design.apply(-1.0, -1.0),
        to_design.apply(1.0, -1.0),
        to_design.apply(-1.0, 1.0),
        to_design.apply(1.0, 1.0),
    ];
    let (mut minx, mut maxx) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut miny, mut maxy) = (f32::INFINITY, f32::NEG_INFINITY);
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
        return;
    }

    let (twf, thf) = (img.width() as f32, img.height() as f32);
    for py in y0..y1 {
        for px in x0..x1 {
            let dx = (px as f32 + 0.5 - map.x) / map.sx;
            let dy = ((h_f - (py as f32 + 0.5)) - map.y) / map.sy;
            let (u, v) = inv.apply(dx, dy);
            if !(-1.0..=1.0).contains(&u) || !(-1.0..=1.0).contains(&v) {
                continue;
            }
            // Quad space -> texture UV, matching the GL quad's winding.
            let su = (u + 1.0) * 0.5 * (twf - 1.0);
            let sv = (1.0 - v) * 0.5 * (thf - 1.0);
            let s = sample(img, su, sv);
            let a = s[3] as f32 / 255.0 * color[3];
            if a <= 0.0 {
                continue;
            }
            let d = canvas.get_pixel_mut(px as u32, py as u32);
            let inv_a = 1.0 - a;
            for (c, sc) in s.iter().take(3).enumerate() {
                d.0[c] =
                    ((*sc as f32 * color[c]) * a + d.0[c] as f32 * inv_a).clamp(0.0, 255.0) as u8;
            }
            d.0[3] = 255;
        }
    }
}

/// A puppet object: a deforming mesh drawn with its own texture.
struct Puppet {
    model: hyprwpe_core::mdlv::PuppetModel,
    rig: Rig,
    img: RgbaImage,
    /// Model coordinates -> design units (object transform + `cropoffset`).
    placement: Affine,
    color: [f32; 4],
}

/// The `cropoffset` a model JSON may carry, in model space.
fn model_cropoffset(pkg: &Resources, image_ref: &str) -> [f32; 2] {
    let Some(body) = pkg.get_str(image_ref) else {
        return [0.0, 0.0];
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return [0.0, 0.0];
    };
    let Some(s) = v.get("cropoffset").and_then(|c| c.as_str()) else {
        return [0.0, 0.0];
    };
    let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
    let x = it.next().unwrap_or(0.0);
    let y = it.next().unwrap_or(0.0);
    [x, y]
}

/// Rasterise a puppet mesh: forward-shade each triangle with its flat texture.
///
/// Triangles come from the deformed (skinned) positions; UVs are the mesh's own.
/// A pixel outside every triangle is left untouched, so overlapping layers
/// composite in scene order exactly as the GL path does.
#[allow(clippy::too_many_arguments)]
fn raster_puppet(
    canvas: &mut RgbaImage,
    map: &hyprwpe_render::scene_transform::CanvasMap,
    p: &Puppet,
    t: f32,
    out_h: f32,
) {
    let mut pos = Vec::new();
    deform(
        &p.model.mesh,
        &p.rig.pose(p.model.animations.first(), t),
        &mut pos,
    );
    let (tw, th) = (p.img.width() as f32, p.img.height() as f32);

    // Model -> design -> output pixels (y down).
    let to_px = |mx: f32, my: f32| -> (f32, f32) {
        let (dx, dy) = p.placement.apply(mx, my);
        (map.x + dx * map.sx, out_h - (map.y + dy * map.sy))
    };

    for tri in p.model.mesh.indices.as_chunks::<3>().0 {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (Some(a), Some(b), Some(c)) = (pos.get(i0), pos.get(i1), pos.get(i2)) else {
            continue;
        };
        let (Some(uv0), Some(uv1), Some(uv2)) = (
            p.model.mesh.uvs.get(i0),
            p.model.mesh.uvs.get(i1),
            p.model.mesh.uvs.get(i2),
        ) else {
            continue;
        };
        let p0 = to_px(a[0], a[1]);
        let p1 = to_px(b[0], b[1]);
        let p2 = to_px(c[0], c[1]);

        let area = (p1.0 - p0.0) * (p2.1 - p0.1) - (p1.1 - p0.1) * (p2.0 - p0.0);
        if !area.is_finite() || area.abs() < 1e-6 {
            continue;
        }
        let (minx, maxx) = (p0.0.min(p1.0).min(p2.0), p0.0.max(p1.0).max(p2.0));
        let (miny, maxy) = (p0.1.min(p1.1).min(p2.1), p0.1.max(p1.1).max(p2.1));
        let x0 = (minx.floor().max(0.0)) as i64;
        let x1 = (maxx.ceil().min(canvas.width() as f32)) as i64;
        let y0 = (miny.floor().max(0.0)) as i64;
        let y1 = (maxy.ceil().min(canvas.height() as f32)) as i64;

        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let w0 = ((p1.0 - fx) * (p2.1 - fy) - (p1.1 - fy) * (p2.0 - fx)) / area;
                let w1 = ((p2.0 - fx) * (p0.1 - fy) - (p2.1 - fy) * (p0.0 - fx)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let u = w0 * uv0[0] + w1 * uv1[0] + w2 * uv2[0];
                let v = w0 * uv0[1] + w1 * uv1[1] + w2 * uv2[1];
                let s = sample(&p.img, u * (tw - 1.0), v * (th - 1.0));
                let alpha = s[3] as f32 / 255.0 * p.color[3];
                if alpha <= 0.0 {
                    continue;
                }
                let d = canvas.get_pixel_mut(px as u32, py as u32);
                let inv_a = 1.0 - alpha;
                for (ci, sc) in s.iter().take(3).enumerate() {
                    d.0[ci] = ((*sc as f32 * p.color[ci]) * alpha + d.0[ci] as f32 * inv_a)
                        .clamp(0.0, 255.0) as u8;
                }
                d.0[3] = 255;
            }
        }
    }
}

/// Resolve a particle definition (and its children) into runnable systems.
fn load_particle_systems(
    pkg: &Resources,
    root_ref: &str,
    placement: Affine,
    color: [f32; 4],
) -> Vec<ParticleSys> {
    let mut out = Vec::new();
    let mut queue = vec![root_ref.to_string()];
    let mut seen = std::collections::HashSet::new();
    let mut seed = 0x1234_5678u32;
    while let Some(path) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(body) = pkg.get_str(&path) else {
            continue;
        };
        let Ok(system) = particle_def::parse(&body) else {
            continue;
        };
        for c in &system.children {
            queue.push(c.clone());
        }
        let Some(mat) = system.material.clone() else {
            continue;
        };
        let Some(mat_body) = pkg.get_str(&mat) else {
            continue;
        };
        let Some(tex) = particle_def::material_texture(&mat_body) else {
            continue;
        };
        let Some(entry) = resolve_texture_entry(pkg, &tex) else {
            continue;
        };
        let Some(img) = load_rgba(pkg, &entry) else {
            continue;
        };
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let cps = system.control_points.clone();
        out.push(ParticleSys {
            sim: ParticleSim::new(system, cps, seed),
            img,
            placement,
            color,
        });
    }
    out
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
    // Optional time (seconds) at which to sample property animations; lets a
    // single scene be rendered at several frames and compared.
    let t: f32 = args
        .get(6)
        .and_then(|a| a.parse().ok())
        .or_else(|| {
            std::env::var("HYPRWPE_TIME")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0.0);

    let pkg = Resources::open(std::path::Path::new(pkg_path)).expect("open pkg");
    let scene_json = pkg.get_str("scene.json").expect("scene.json");
    let scene = Scene::from_json_str(&scene_json).expect("scene json");
    let raw: serde_json::Value = serde_json::from_str(&scene_json).unwrap();

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

    let world = animated_world_transforms(&scene.objects, &scene.animations, t);
    let mut layers: Vec<Layer> = Vec::new();
    let mut puppets: Vec<Puppet> = Vec::new();
    let mut particles: Vec<ParticleSys> = Vec::new();
    for (i, obj) in scene.objects.iter().enumerate() {
        // Particle objects carry no quad; they simulate sprites.
        if let Some(pref) = obj.particle_path() {
            if !obj.is_visible() {
                continue;
            }
            let color_rgb = obj
                .color
                .map(|c| [c.x(), c.y(), c.z()])
                .unwrap_or([1.0, 1.0, 1.0]);
            let anims = scene.animations.get(i).cloned().unwrap_or_default();
            let alpha = animated_alpha(obj, &anims, t);
            particles.extend(load_particle_systems(
                &pkg,
                &pref,
                world[i],
                [color_rgb[0], color_rgb[1], color_rgb[2], alpha],
            ));
            continue;
        }
        if obj.kind() != ObjectKind::Image || !obj.is_visible() {
            continue;
        }
        let Some(image_ref) = obj.image_path() else {
            continue;
        };

        // A model that names a `puppet` is a deforming mesh: rasterise its
        // triangles rather than drawing its texture on a quad.
        if let Some(mdl_path) = resolve_puppet_file(&pkg, &image_ref) {
            let Some(raw) = pkg.get(&mdl_path) else {
                continue;
            };
            let Ok(model) = hyprwpe_core::mdlv::parse(&raw) else {
                eprintln!("  unparsable puppet: {mdl_path}");
                continue;
            };
            let Some(entry) = resolve_texture_file(&pkg, &image_ref) else {
                continue;
            };
            let Some(img) = load_rgba(&pkg, &entry) else {
                continue;
            };
            let color_rgb = obj
                .color
                .map(|c| [c.x(), c.y(), c.z()])
                .unwrap_or([1.0, 1.0, 1.0]);
            let anims = scene.animations.get(i).cloned().unwrap_or_default();
            let alpha = animated_alpha(obj, &anims, t);
            let crop = model_cropoffset(&pkg, &image_ref);
            let rig = Rig::new(&model);
            println!(
                "  puppet {mdl_path}: {} verts / {} tris, {} bones, {} anims, crop {crop:?}",
                model.mesh.positions.len(),
                model.mesh.indices.len() / 3,
                model.bones.len(),
                model.animations.len()
            );
            puppets.push(Puppet {
                model,
                rig,
                img,
                placement: puppet_placement(&world[i], crop),
                color: [color_rgb[0], color_rgb[1], color_rgb[2], alpha],
            });
            continue;
        }

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
        let anims = scene.animations.get(i).cloned().unwrap_or_default();
        let alpha = animated_alpha(obj, &anims, t);

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
        raster_quad(&mut canvas, &map, &l.img, &l.to_design, l.color, w_f, h_f);
    }

    // Particle systems: simulate deterministically to the requested time and
    // draw each sprite through the same quad path the image layers use.
    for ps in &mut particles {
        let sprites = ps.sim.advance_to(t);
        for s in &sprites {
            let to_design = particle_sprite_transform(&ps.placement, s.pos, s.rotation, s.size);
            let color = [
                s.color[0] * ps.color[0],
                s.color[1] * ps.color[1],
                s.color[2] * ps.color[2],
                s.alpha * ps.color[3],
            ];
            raster_quad(&mut canvas, &map, &ps.img, &to_design, color, w_f, h_f);
        }
    }

    // Puppets are drawn after the quads that precede them; a puppet's own z
    // order among layers is not modelled here (the live renderer keeps scene
    // order), which is enough to check placement and deformation.
    for p in &puppets {
        raster_puppet(&mut canvas, &map, p, t, h_f);
    }

    canvas.save(out_path).expect("save");
    println!(
        "wrote {out_path} ({} layers, {} puppets, {} particles)",
        layers.len(),
        puppets.len(),
        particles.len()
    );
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
        let px = img
            .get_pixel(clamp(ix, w - 1) as u32, clamp(iy, h - 1) as u32)
            .0;
        [px[0] as f32, px[1] as f32, px[2] as f32, px[3] as f32]
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
