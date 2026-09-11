use hyprwpe_core::assets::Resources;
use hyprwpe_core::particle as pdef;
use hyprwpe_core::scene::Scene;
use hyprwpe_core::tex::TexImage;
use std::path::Path;

fn is_image_file(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    [".tex", ".png", ".jpg", ".jpeg", ".tga", ".bmp"]
        .iter()
        .any(|e| l.ends_with(e))
}

fn resolve_entry(res: &Resources, name: &str) -> Option<String> {
    if is_image_file(name) {
        for c in [format!("materials/{name}"), name.to_string()] {
            if res.get(&c).is_some() {
                return Some(c);
            }
        }
        return None;
    }
    for ext in [".tex", ".png", ".jpg", ".jpeg", ".tga", ".bmp", ""] {
        let c = format!("materials/{name}{ext}");
        if res.get(&c).is_some() {
            return Some(c);
        }
    }
    None
}

fn main() {
    let p = std::env::args().nth(1).unwrap();
    let res = Resources::open(Path::new(&p)).unwrap();
    let scene = Scene::from_json_str(&res.get_str("scene.json").unwrap()).unwrap();
    println!("assets: {}", res.has_assets());
    for (i, o) in scene.objects.iter().enumerate() {
        let Some(pref) = o.particle_path() else {
            continue;
        };
        println!("\nobject {i}: particle = {pref}");
        let Some(body) = res.get_str(&pref) else {
            println!("  !! definition missing from package");
            continue;
        };
        let sys = pdef::parse(&body).unwrap();
        println!(
            "  emitters={} inits={} ops={} renderers={} maxcount={} starttime={}",
            sys.emitters.len(),
            sys.initializers.len(),
            sys.operators.len(),
            sys.renderers.len(),
            sys.maxcount,
            sys.starttime
        );
        let Some(mat) = sys.material.clone() else {
            println!("  !! no material");
            continue;
        };
        println!("  material = {mat}");
        let Some(mb) = res.get_str(&mat) else {
            println!("  !! material missing");
            continue;
        };
        let Some(tex) = pdef::material_texture(&mb) else {
            println!("  !! no texture in material");
            continue;
        };
        println!("  texture name = {tex}");
        let Some(entry) = resolve_entry(&res, &tex) else {
            println!("  !! texture unresolved");
            continue;
        };
        println!(
            "  entry = {entry} ({} bytes)",
            res.get(&entry).unwrap().len()
        );
        let (raw, side) = res.texture(&entry).unwrap();
        match TexImage::parse_with_sidecar(&raw, side.as_deref()).and_then(|t| t.to_rgba_image()) {
            Ok(img) => println!("  decoded {}x{}", img.width(), img.height()),
            Err(e) => println!("  !! decode error: {e}"),
        }
    }
}
