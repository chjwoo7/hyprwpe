//! Measure the effect pipeline against the real corpus.
//!
//! For every effect a scene references, walk the chain the engine defines -
//! `effects/<n>/effect.json` -> `passes[].material` -> the material's
//! `passes[].shader` -> `shaders/<name>.{frag,vert}` - expand its `#include`s,
//! assemble the source, and count the uniforms. What this proves is that the
//! *host side* of the shader contract works for the whole library; it does not
//! compile GLSL (that needs a GL context).
//!
//! Usage: cargo run -p hyprwpe-core --example validate_effects -- <workshop-dir>

use hyprwpe_core::assets::Resources;
use hyprwpe_core::effect;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "~/.steam/root/steamapps/workshop/content/431960".into());
    let root = PathBuf::from(shellexpand(&root));

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read workshop dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("scene.pkg").is_file())
        .collect();
    dirs.sort();

    let mut effects_seen: BTreeSet<String> = BTreeSet::new();
    let mut shaders_ok = 0usize;
    let mut shaders_failed: BTreeMap<String, String> = BTreeMap::new();
    let mut total_uniforms = 0usize;
    let mut bound_uniforms = 0usize;
    let mut no_assets = 0usize;

    for dir in &dirs {
        let Ok(res) = Resources::open(&dir.join("scene.pkg")) else {
            continue;
        };
        if !res.has_assets() {
            no_assets += 1;
        }
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
                effects_seen.insert(file.clone());
                let Some(body) = res.get_str(file) else {
                    shaders_failed
                        .entry(file.clone())
                        .or_insert("effect.json missing".into());
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
                for mat in mats {
                    let Some(mb) = res.get_str(&mat) else {
                        shaders_failed
                            .entry(mat.clone())
                            .or_insert("material missing".into());
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
                            let stage = effect::stage_of(&path);
                            let lookup = |n: &str| res.shader(n);
                            match effect::expand_includes(&src, &lookup) {
                                Ok(expanded) => {
                                    let assembled = effect::assemble(&expanded, stage, &[]);
                                    let binds = effect::uniform_bindings(&assembled);
                                    total_uniforms += binds.len();
                                    bound_uniforms +=
                                        binds.iter().filter(|b| b.material.is_some()).count();
                                    let _ = assembled;
                                    shaders_ok += 1;
                                }
                                Err(e) => {
                                    shaders_failed.insert(path.clone(), format!("{e}"));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    println!("scenes scanned      : {}", dirs.len());
    println!("distinct effects    : {}", effects_seen.len());
    println!("shader stages ok    : {shaders_ok}");
    println!("shader stages failed: {}", shaders_failed.len());
    println!(
        "uniforms seen       : {total_uniforms} ({bound_uniforms} bound to a material constant)"
    );
    if no_assets > 0 {
        println!("scenes with no engine assets found: {no_assets}");
    }
    if !shaders_failed.is_empty() {
        println!("\nfailures:");
        for (k, v) in shaders_failed.iter().take(25) {
            println!("  {k}: {v}");
        }
    }
}

fn shellexpand(p: &str) -> String {
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
