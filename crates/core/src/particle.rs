//! Wallpaper Engine particle system definitions (`particles/**/*.json`).
//!
//! A particle system is **declarative data**, not code: a list of emitters,
//! initializers, operators and renderers, each naming a behaviour and carrying
//! its parameters. The whole vocabulary in the local corpus is small and closed
//! (`tools/particle_vocab.py` measures it):
//!
//! * emitters: `sphererandom`, `boxrandom`
//! * initializers: `lifetimerandom`, `sizerandom`, `colorrandom`,
//!   `velocityrandom`, `rotationrandom`, `turbulentvelocityrandom`,
//!   `alpharandom`, `angularvelocityrandom`, `mapsequencebetweencontrolpoints`,
//!   `mapsequencearoundcontrolpoint`
//! * operators: `movement`, `alphafade`, `alphachange`, `sizechange`,
//!   `colorchange`, `oscillatealpha`, `oscillatesize`, `oscillateposition`,
//!   `turbulence`, `angularmovement`, `vortex`, `controlpointattract`
//! * renderers: `sprite`, `spritetrail`, `ropetrail`, `rope`
//!
//! So this module deliberately does **not** enumerate a struct per behaviour.
//! A behaviour the simulator does not know is data, not a parse failure, and a
//! parameter added by a newer editor version is simply unread — which is what
//! keeps one parser working across a whole library instead of one wallpaper.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

/// One named behaviour with its parameters.
///
/// Parameters are kept as JSON values and read by name on demand, so a
/// parameter that never appears costs nothing and an unknown one is ignored.
#[derive(Debug, Clone, Default)]
pub struct Item {
    pub name: String,
    pub params: serde_json::Map<String, serde_json::Value>,
}

impl Item {
    pub fn has(&self, key: &str) -> bool {
        self.params.contains_key(key)
    }

    /// A number parameter. The editor writes numbers as JSON numbers, but a
    /// scalar bound to a material constant can also arrive as a one-element
    /// string, so both are accepted.
    pub fn f32(&self, key: &str) -> Option<f32> {
        match self.params.get(key)? {
            serde_json::Value::Number(n) => n.as_f64().map(|v| v as f32),
            serde_json::Value::String(s) => s
                .split_whitespace()
                .next()
                .and_then(|t| t.parse::<f32>().ok()),
            _ => None,
        }
    }

    pub fn f32_or(&self, key: &str, default: f32) -> f32 {
        self.f32(key).unwrap_or(default)
    }

    pub fn u32(&self, key: &str) -> Option<u32> {
        self.f32(key).map(|v| v.max(0.0) as u32)
    }

    pub fn usize_or(&self, key: &str, default: usize) -> usize {
        self.u32(key).map(|v| v as usize).unwrap_or(default)
    }

    /// A 3-component parameter. Accepts `"1 2 3"`, a bare number (all three
    /// components equal), or an array.
    pub fn vec3(&self, key: &str) -> Option<[f32; 3]> {
        let v = self.params.get(key)?;
        match v {
            serde_json::Value::String(s) => {
                let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
                let x = it.next()?;
                let y = it.next().unwrap_or(x);
                let z = it.next().unwrap_or(x);
                Some([x, y, z])
            }
            serde_json::Value::Number(n) => {
                let x = n.as_f64()? as f32;
                Some([x, x, x])
            }
            serde_json::Value::Array(a) => {
                let mut it = a.iter().filter_map(|e| e.as_f64().map(|v| v as f32));
                let x = it.next()?;
                let y = it.next().unwrap_or(x);
                let z = it.next().unwrap_or(x);
                Some([x, y, z])
            }
            _ => None,
        }
    }

    pub fn vec3_or(&self, key: &str, default: [f32; 3]) -> [f32; 3] {
        self.vec3(key).unwrap_or(default)
    }

    pub fn f32_pair(&self, a: &str, b: &str, default: [f32; 2]) -> [f32; 2] {
        [self.f32_or(a, default[0]), self.f32_or(b, default[1])]
    }

    /// `min`/`max` bound, which almost every `…random` initializer carries.
    pub fn min_max(&self, default: [f32; 2]) -> [f32; 2] {
        self.f32_pair("min", "max", default)
    }
}

/// A `{ "id": n, "name": "..." }` child reference to another system.
#[derive(Debug, Clone, Deserialize)]
struct ChildRef {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "lowercase")]
struct RawSystem {
    emitter: Vec<serde_json::Value>,
    initializer: Vec<serde_json::Value>,
    operator: Vec<serde_json::Value>,
    renderer: Vec<serde_json::Value>,
    controlpoint: Vec<serde_json::Value>,
    children: Option<Vec<ChildRef>>,
    material: Option<String>,
    maxcount: Option<serde_json::Value>,
    starttime: Option<serde_json::Value>,
}

/// A parsed particle system.
#[derive(Debug, Clone, Default)]
pub struct System {
    pub emitters: Vec<Item>,
    pub initializers: Vec<Item>,
    pub operators: Vec<Item>,
    pub renderers: Vec<Item>,
    /// Child systems, resolved by the loader (each has its own definition).
    pub children: Vec<String>,
    /// Control-point offsets, indexed by control-point id. A particle can be
    /// attached to one (a character's hand, say) instead of the object origin.
    pub control_points: Vec<[f32; 3]>,
    /// Material JSON naming the sprite texture.
    pub material: Option<String>,
    /// Upper bound on live particles for this system.
    pub maxcount: usize,
    /// Delay before emission begins, in seconds.
    pub starttime: f32,
}

impl System {
    /// Whether the system has anything to draw. A system with no emitter (a
    /// pure child) spawns nothing on its own.
    pub fn is_empty(&self) -> bool {
        self.emitters.is_empty()
    }
}

fn item_from(v: &serde_json::Value) -> Option<Item> {
    let obj = v.as_object()?;
    let name = obj.get("name")?.as_str()?.to_string();
    // Keep every parameter except the identity fields.
    let params = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "name" && k.as_str() != "id")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<serde_json::Map<_, _>>();
    Some(Item { name, params })
}

fn items(list: &[serde_json::Value]) -> Vec<Item> {
    list.iter().filter_map(item_from).collect()
}

/// Parse a particle definition. Missing sections are empty, never an error.
pub fn parse(json: &str) -> Result<System> {
    let json = json.strip_prefix('\u{feff}').unwrap_or(json);
    let raw: RawSystem = serde_json::from_str(json).context("parsing particle definition")?;

    let maxcount = raw
        .maxcount
        .as_ref()
        .and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
            _ => None,
        })
        .unwrap_or(0) as usize;
    let starttime = raw
        .starttime
        .as_ref()
        .and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_f64().map(|f| f as f32),
            serde_json::Value::String(s) => s.trim().parse::<f32>().ok(),
            _ => None,
        })
        .unwrap_or(0.0);

    Ok(System {
        emitters: items(&raw.emitter),
        initializers: items(&raw.initializer),
        operators: items(&raw.operator),
        renderers: items(&raw.renderer),
        children: raw
            .children
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| c.name)
            .collect(),
        control_points: raw
            .controlpoint
            .iter()
            .filter_map(|c| {
                let obj = c.as_object()?;
                let off = obj.get("offset")?;
                match off {
                    serde_json::Value::String(s) => {
                        let mut it = s.split_whitespace().filter_map(|t| t.parse::<f32>().ok());
                        let x = it.next().unwrap_or(0.0);
                        let y = it.next().unwrap_or(0.0);
                        let z = it.next().unwrap_or(0.0);
                        Some([x, y, z])
                    }
                    _ => None,
                }
            })
            .collect(),
        material: raw.material,
        maxcount: if maxcount == 0 { 1000 } else { maxcount },
        starttime,
    })
}

/// A material's sprite texture, if it resolves to one.
///
/// A particle material is the same shape as an image material: a `passes`
/// array whose first pass carries `textures`.
pub fn material_texture(material_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(material_json).ok()?;
    let first = v
        .get("passes")
        .and_then(|p| p.as_array())
        .and_then(|p| p.first())?;
    // `textures` may live on the pass or be inherited from a parent material,
    // so also scan `texturescombos` and the top-level `textures`.
    let from_pass = first
        .get("textures")
        .and_then(|t| t.as_array())
        .and_then(|t| t.first())
        .and_then(|v| v.as_str());
    let from_root = v
        .get("textures")
        .and_then(|t| t.as_array())
        .and_then(|t| t.first())
        .and_then(|v| v.as_str());
    from_pass.or(from_root).map(|s| s.to_string())
}

/// Support all sections for a name, for the coverage report.
pub fn vocabularies() -> BTreeMap<&'static str, &'static [&'static str]> {
    let mut m = BTreeMap::new();
    m.insert("emitter", &["sphererandom", "boxrandom"][..]);
    m.insert(
        "initializer",
        &[
            "lifetimerandom",
            "sizerandom",
            "colorrandom",
            "velocityrandom",
            "rotationrandom",
            "turbulentvelocityrandom",
            "alpharandom",
            "angularvelocityrandom",
            "mapsequencebetweencontrolpoints",
            "mapsequencearoundcontrolpoint",
        ][..],
    );
    m.insert(
        "operator",
        &[
            "movement",
            "alphafade",
            "alphachange",
            "sizechange",
            "colorchange",
            "oscillatealpha",
            "oscillatesize",
            "oscillateposition",
            "turbulence",
            "angularmovement",
            "vortex",
            "controlpointattract",
        ][..],
    );
    m.insert(
        "renderer",
        &["sprite", "spritetrail", "ropetrail", "rope"][..],
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOG: &str = r#"{
        "emitter": [
            {"directions": "1 0.2 0", "distancemax": 512, "distancemin": 0,
             "id": 8, "name": "sphererandom", "origin": "0 0 0", "rate": 1.5}
        ],
        "initializer": [
            {"id": 2, "max": 5, "min": 3, "name": "lifetimerandom"},
            {"id": 4, "max": "80 0 0", "min": "25 0 0", "name": "velocityrandom"}
        ],
        "operator": [
            {"gravity": "0 0 0", "id": 9, "name": "movement"},
            {"fadeintime": 0.5, "id": 10, "name": "alphafade"}
        ],
        "renderer": [{"id": 1, "name": "sprite"}],
        "material": "materials/presets/fog1.json",
        "maxcount": 20,
        "starttime": 2,
        "children": null
    }"#;

    #[test]
    fn parses_the_real_shape() {
        let s = parse(FOG).expect("parse");
        assert_eq!(s.emitters.len(), 1);
        assert_eq!(s.initializers.len(), 2);
        assert_eq!(s.operators.len(), 2);
        assert_eq!(s.renderers.len(), 1);
        assert_eq!(s.maxcount, 20);
        assert!((s.starttime - 2.0).abs() < 1e-6);
        assert_eq!(s.material.as_deref(), Some("materials/presets/fog1.json"));
    }

    #[test]
    fn parameters_read_by_name_with_types() {
        let s = parse(FOG).unwrap();
        let em = &s.emitters[0];
        assert_eq!(em.name, "sphererandom");
        assert_eq!(em.vec3("directions"), Some([1.0, 0.2, 0.0]));
        assert!((em.f32_or("distancemax", 0.0) - 512.0).abs() < 1e-6);
        assert!((em.f32_or("rate", 0.0) - 1.5).abs() < 1e-6);
        // A vector parameter written as a string.
        let vel = &s.initializers[1];
        assert_eq!(vel.vec3("min"), Some([25.0, 0.0, 0.0]));
        assert_eq!(vel.vec3("max"), Some([80.0, 0.0, 0.0]));
        // min/max pair helper.
        assert_eq!(s.initializers[0].min_max([0.0, 1.0]), [3.0, 5.0]);
    }

    #[test]
    fn an_unknown_parameter_or_behaviour_is_not_an_error() {
        let json = r#"{
            "initializer": [{"name": "some_future_initializer", "newparam": 3}],
            "emitter": [{"name": "sphererandom", "rate": 1}]
        }"#;
        let s = parse(json).expect("unknown behaviour must still parse");
        assert_eq!(s.initializers[0].name, "some_future_initializer");
        assert_eq!(s.initializers[0].f32("newparam"), Some(3.0));
    }

    #[test]
    fn missing_sections_default_to_empty() {
        let s = parse("{}").expect("empty definition");
        assert!(s.is_empty());
        assert_eq!(s.maxcount, 1000, "a missing maxcount gets a sane bound");
    }

    #[test]
    fn material_texture_reads_the_first_pass() {
        let m = r#"{"passes":[{"textures":["particle/glow"]}]}"#;
        assert_eq!(material_texture(m).as_deref(), Some("particle/glow"));
        assert_eq!(material_texture("{}"), None);
    }
}
