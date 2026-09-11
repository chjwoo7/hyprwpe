//! Measure the user-property layer against the real library.
//!
//! Every wallpaper declares its settings in `general.properties` - in
//! `project.json` next to the package, and inside the package's own
//! `scene.json`. This walks both, builds a `PropertySet`, and coerces each
//! property's own default back through `coerce` to prove the round trip is a
//! no-op. A property whose default does not survive coercion is one the panel
//! would silently change the moment it is opened.
//!
//! Usage: cargo run -p hyprwpe-core --example validate_properties [workshop-dir]

use hyprwpe_core::assets::Resources;
use hyprwpe_core::properties::PropertySet;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "~/.steam/root/steamapps/workshop/content/431960".into());
    let root = PathBuf::from(expand(&root));

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read workshop dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut items = 0usize;
    let mut with_props = 0usize;
    let mut total = 0usize;
    let mut editable = 0usize;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut from_package = 0usize;
    let mut unstable: Vec<String> = Vec::new();
    // Bindings: how many declared properties a scene actually wires up, and how
    // many bindings are SceneScript (which this crate counts but cannot run).
    let mut bound: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut declared: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut script_bindings = 0usize;
    let mut resolved_fields = 0usize;

    for dir in &dirs {
        let Project { id, text } = read_project(dir);
        let Some(text) = text else { continue };
        items += 1;
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let mut set = PropertySet::from_document(&doc);

        // The package's own scene.json is the authoritative source the runtime
        // reads, and it is also where the bindings live, so prefer it.
        let mut scene_doc = doc.clone();
        let pkg = dir.join("scene.pkg");
        if pkg.is_file() {
            if let Ok(res) = Resources::open(&pkg) {
                if let Some(scene_text) = res.get_str("scene.json") {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&scene_text) {
                        scene_doc = parsed;
                        let from_scene = PropertySet::from_document(&scene_doc);
                        if !from_scene.is_empty() {
                            from_package += 1;
                            set = from_scene;
                        }
                    }
                }
            }
        }

        // Which declared properties does the scene actually wire up, and how
        // many bindings are SceneScript?
        for p in &set.properties {
            if binding_used(&scene_doc, &p.key) {
                bound.insert(format!("{id}:{}", p.key));
                resolved_fields += 1;
            }
        }
        script_bindings += PropertySet::count_scripts(&scene_doc);

        if set.is_empty() {
            continue;
        }
        with_props += 1;
        for p in &set.properties {
            total += 1;
            declared.insert(format!("{id}:{}", p.key));
            if p.kind.is_editable() {
                editable += 1;
            }
            *kinds.entry(p.kind.as_str().to_string()).or_default() += 1;
            // The declaration's own default must survive its own coercion.
            match p.coerce(&p.value) {
                Some(v) if v == p.value => {}
                Some(v) => unstable.push(format!(
                    "{id}:{} {} {:?} -> {:?}",
                    p.key,
                    p.kind.as_str(),
                    p.value,
                    v
                )),
                None => unstable.push(format!(
                    "{id}:{} {} {:?} -> refused",
                    p.key,
                    p.kind.as_str(),
                    p.value
                )),
            }
        }
    }

    let unbound: Vec<&String> = declared.difference(&bound).collect();
    println!("items scanned        : {items}");
    println!("items with properties: {with_props}");
    println!("properties           : {total}");
    println!("  editable           : {editable}");
    println!("  read from a package: {from_package} items");
    println!("by type:");
    for (k, n) in &kinds {
        println!("  {:14} {n}", if k.is_empty() { "(no type)" } else { k });
    }
    println!(
        "declared properties wired to a field: {} of {}",
        bound.len(),
        declared.len()
    );
    println!("  bindings resolved (fields)        : {resolved_fields}");
    println!("  SceneScript bindings (counted, not run): {script_bindings}");
    if !unbound.is_empty() {
        println!(
            "  declared but never bound (the panel still shows them): {}",
            unbound.len()
        );
        for k in unbound.iter().take(6) {
            println!("    {k}");
        }
    }
    println!(
        "defaults adjusted on load (clamped, or filled from the first option): {}",
        unstable.len()
    );
    for u in unstable.iter().take(20) {
        println!("  {u}");
    }
}

/// Whether a scene document binds any field to a property.
fn binding_used(doc: &serde_json::Value, key: &str) -> bool {
    match doc {
        serde_json::Value::Object(o) => {
            if let Some(serde_json::Value::String(s)) = o.get("user") {
                if s == key {
                    return true;
                }
            }
            if let Some(serde_json::Value::Object(u)) = o.get("user") {
                if u.get("name").and_then(|n| n.as_str()) == Some(key) {
                    return true;
                }
            }
            o.values().any(|v| binding_used(v, key))
        }
        serde_json::Value::Array(a) => a.iter().any(|v| binding_used(v, key)),
        _ => false,
    }
}

struct Project {
    id: String,
    text: Option<String>,
}

fn read_project(dir: &std::path::Path) -> Project {
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = std::fs::read(dir.join("project.json"))
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    Project { id, text }
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
