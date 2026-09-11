//! Schema and scene graph parser for Wallpaper Engine `scene.json`.
//!
//! A `scene.json` file describes the scene graph: a camera, global environment
//! properties (`general`), and a list of `objects` (images, particles, text,
//! sounds, lights). Objects can form a hierarchy via `parent` IDs and share
//! common transform properties (origin, scale, angles, size, color, alpha).

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;

/// A 3D vector supporting both array (`[x, y, z]`) and string (`"x y z"`) serialization.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3(pub [f32; 3]);

impl Vec3 {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3([x, y, z])
    }

    pub fn x(&self) -> f32 {
        self.0[0]
    }
    pub fn y(&self) -> f32 {
        self.0[1]
    }
    pub fn z(&self) -> f32 {
        self.0[2]
    }
}

impl<'de> Deserialize<'de> for Vec3 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Vec3Visitor;
        impl<'de> Visitor<'de> for Vec3Visitor {
            type Value = Vec3;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a 3D vector as an array [x, y, z] or string 'x y z'")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Vec3, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let x = seq.next_element()?.unwrap_or(0.0);
                let y = seq.next_element()?.unwrap_or(0.0);
                let z = seq.next_element()?.unwrap_or(0.0);
                Ok(Vec3([x, y, z]))
            }

            fn visit_str<E>(self, v: &str) -> Result<Vec3, E>
            where
                E: de::Error,
            {
                let nums: Vec<f32> = v
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                let x = nums.first().copied().unwrap_or(0.0);
                let y = nums.get(1).copied().unwrap_or(0.0);
                let z = nums.get(2).copied().unwrap_or(0.0);
                Ok(Vec3([x, y, z]))
            }

            fn visit_f64<E>(self, v: f64) -> Result<Vec3, E>
            where
                E: de::Error,
            {
                let f = v as f32;
                Ok(Vec3([f, f, f]))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Vec3, E>
            where
                E: de::Error,
            {
                let f = v as f32;
                Ok(Vec3([f, f, f]))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Vec3, E>
            where
                E: de::Error,
            {
                let f = v as f32;
                Ok(Vec3([f, f, f]))
            }

            fn visit_map<M>(self, mut map: M) -> Result<Vec3, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                // Consume EVERY entry: a property may carry both `value` and
                // `animation`, and returning as soon as `value` was seen would
                // abandon the rest of the map (serde then reports a "trailing
                // comma"). Later keys simply overwrite earlier ones.
                let mut res = Vec3::default();
                while let Some((k, v)) = map.next_entry::<String, serde_json::Value>()? {
                    match k.as_str() {
                        "value" => {
                            if let Some(s) = v.as_str() {
                                res = parse_vec3_str(s);
                            } else if let Some(arr) = v.as_array() {
                                res = Vec3([
                                    arr.first().and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                                    arr.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                                    arr.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                                ]);
                            }
                        }
                        "x" => res.0[0] = v.as_f64().unwrap_or(0.0) as f32,
                        "y" => res.0[1] = v.as_f64().unwrap_or(0.0) as f32,
                        "z" => res.0[2] = v.as_f64().unwrap_or(0.0) as f32,
                        _ => {}
                    }
                }
                Ok(res)
            }
        }
        deserializer.deserialize_any(Vec3Visitor)
    }
}

/// Parse a `"x y z"` (or `"x,y,z"`) vector string; missing parts are zero.
fn parse_vec3_str(v: &str) -> Vec3 {
    let nums: Vec<f32> = v
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    Vec3([
        nums.first().copied().unwrap_or(0.0),
        nums.get(1).copied().unwrap_or(0.0),
        nums.get(2).copied().unwrap_or(0.0),
    ])
}

/// A 2D vector supporting both array (`[x, y]`) and string (`"x y"`) serialization.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2(pub [f32; 2]);

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Vec2([x, y])
    }

    pub fn x(&self) -> f32 {
        self.0[0]
    }
    pub fn y(&self) -> f32 {
        self.0[1]
    }
}

impl<'de> Deserialize<'de> for Vec2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Vec2Visitor;
        impl<'de> Visitor<'de> for Vec2Visitor {
            type Value = Vec2;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a 2D vector as an array [x, y] or string 'x y'")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Vec2, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let x = seq.next_element()?.unwrap_or(0.0);
                let y = seq.next_element()?.unwrap_or(0.0);
                Ok(Vec2([x, y]))
            }

            fn visit_str<E>(self, v: &str) -> Result<Vec2, E>
            where
                E: de::Error,
            {
                let nums: Vec<f32> = v
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                let x = nums.first().copied().unwrap_or(0.0);
                let y = nums.get(1).copied().unwrap_or(0.0);
                Ok(Vec2([x, y]))
            }

            fn visit_f64<E>(self, v: f64) -> Result<Vec2, E>
            where
                E: de::Error,
            {
                let f = v as f32;
                Ok(Vec2([f, f]))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Vec2, E>
            where
                E: de::Error,
            {
                let f = v as f32;
                Ok(Vec2([f, f]))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Vec2, E>
            where
                E: de::Error,
            {
                let f = v as f32;
                Ok(Vec2([f, f]))
            }

            fn visit_map<M>(self, mut map: M) -> Result<Vec2, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                // See `Vec3`: every entry must be consumed, because a property
                // can carry `value` and `animation` together.
                let mut res = Vec2::default();
                while let Some((k, v)) = map.next_entry::<String, serde_json::Value>()? {
                    match k.as_str() {
                        "value" => {
                            if let Some(s) = v.as_str() {
                                res = parse_vec2_str(s);
                            } else if let Some(arr) = v.as_array() {
                                res = Vec2([
                                    arr.first().and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                                    arr.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                                ]);
                            }
                        }
                        "x" => res.0[0] = v.as_f64().unwrap_or(0.0) as f32,
                        "y" => res.0[1] = v.as_f64().unwrap_or(0.0) as f32,
                        _ => {}
                    }
                }
                Ok(res)
            }
        }
        deserializer.deserialize_any(Vec2Visitor)
    }
}

/// Parse an `"x y"` (or `"x,y"`) vector string; missing parts are zero.
fn parse_vec2_str(v: &str) -> Vec2 {
    let nums: Vec<f32> = v
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    Vec2([
        nums.first().copied().unwrap_or(0.0),
        nums.get(1).copied().unwrap_or(0.0),
    ])
}

pub fn deserialize_opt_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptBoolVisitor;
    impl<'de> Visitor<'de> for OptBoolVisitor {
        type Value = Option<bool>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a boolean, integer, string, or object with 'value'")
        }
        fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v != 0))
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v != 0))
        }
        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v != 0.0))
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            match v.to_ascii_lowercase().as_str() {
                "true" | "1" => Ok(Some(true)),
                "false" | "0" => Ok(Some(false)),
                _ => Ok(None),
            }
        }
        fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            let mut val = None;
            while let Some((k, v)) = access.next_entry::<String, serde_json::Value>()? {
                if k == "value" {
                    if let Some(b) = v.as_bool() {
                        val = Some(b);
                    } else if let Some(i) = v.as_i64() {
                        val = Some(i != 0);
                    } else if let Some(s) = v.as_str() {
                        val = Some(s == "true" || s == "1");
                    }
                }
            }
            Ok(val.or(Some(true)))
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }
    }
    deserializer.deserialize_any(OptBoolVisitor)
}

pub fn deserialize_opt_f32<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptF32Visitor;
    impl<'de> Visitor<'de> for OptF32Visitor {
        type Value = Option<f32>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a float, integer, string, or object with 'value'")
        }
        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v as f32))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v as f32))
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v as f32))
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(v.parse().ok())
        }
        fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            let mut val = None;
            while let Some((k, v)) = access.next_entry::<String, serde_json::Value>()? {
                if k == "value" {
                    if let Some(f) = v.as_f64() {
                        val = Some(f as f32);
                    } else if let Some(i) = v.as_i64() {
                        val = Some(i as f32);
                    } else if let Some(s) = v.as_str() {
                        val = s.parse().ok();
                    }
                }
            }
            Ok(val)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            deserializer.deserialize_any(self)
        }
    }
    deserializer.deserialize_any(OptF32Visitor)
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Camera {
    #[serde(default)]
    pub center: Option<Vec3>,
    #[serde(default)]
    pub eye: Option<Vec3>,
    #[serde(default)]
    pub up: Option<Vec3>,
    #[serde(default, deserialize_with = "deserialize_opt_f32")]
    pub fov: Option<f32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct General {
    #[serde(default)]
    pub ambientcolor: Option<Vec3>,
    #[serde(default)]
    pub clearcolor: Option<Vec3>,
    /// Whether the scene asks for its `clearcolor` background to be painted
    /// behind the layers.
    #[serde(default, deserialize_with = "deserialize_opt_bool")]
    pub clearenabled: Option<bool>,
    #[serde(default)]
    pub skylightcolor: Option<Vec3>,
    #[serde(default, deserialize_with = "deserialize_opt_bool")]
    pub cameraparallax: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_opt_f32")]
    pub cameraparallaxamount: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_opt_f32")]
    pub cameraparallaxdelay: Option<f32>,
    /// The orthogonal projection canvas, when the scene uses one. Object
    /// `origin` values live in this space; rendering maps this canvas to the
    /// output, preserving aspect, so scenes are laid out in "design units"
    /// rather than output pixels.
    #[serde(default)]
    pub orthogonalprojection: Option<OrthogonalProjection>,
    #[serde(default, deserialize_with = "deserialize_opt_f32")]
    pub zoom: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_opt_f32")]
    pub nearz: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_opt_f32")]
    pub farz: Option<f32>,
}

/// The `general.orthogonalprojection` object from a scene's scenario.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OrthogonalProjection {
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub height: Option<f32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Effect {
    #[serde(default)]
    pub id: Option<u32>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_bool")]
    pub visible: Option<bool>,
    #[serde(default)]
    pub values: Option<serde_json::Value>,
}

/// The inferred kind of a scene object based on which key it contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Image,
    Particle,
    Text,
    Sound,
    Light,
    Other,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Image => "image",
            ObjectKind::Particle => "particle",
            ObjectKind::Text => "text",
            ObjectKind::Sound => "sound",
            ObjectKind::Light => "light",
            ObjectKind::Other => "other",
        }
    }
}

/// A node in the scene graph.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SceneObject {
    #[serde(default)]
    pub id: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub origin: Option<Vec3>,
    #[serde(default)]
    pub scale: Option<Vec3>,
    #[serde(default)]
    pub angles: Option<Vec3>,
    #[serde(default)]
    pub size: Option<Vec2>,
    #[serde(default)]
    pub color: Option<Vec3>,
    #[serde(default, deserialize_with = "deserialize_opt_f32")]
    pub alpha: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_opt_bool")]
    pub visible: Option<bool>,
    #[serde(default)]
    pub parent: Option<u32>,
    #[serde(default, rename = "parallaxDepth")]
    pub parallax_depth: Option<Vec2>,
    #[serde(default)]
    pub effects: Vec<Effect>,

    // Type-specific discriminators:
    #[serde(default)]
    pub image: Option<serde_json::Value>,
    #[serde(default)]
    pub particle: Option<serde_json::Value>,
    #[serde(default)]
    pub text: Option<serde_json::Value>,
    #[serde(default)]
    pub sound: Option<serde_json::Value>,
    #[serde(default)]
    pub light: Option<serde_json::Value>,
}

impl SceneObject {
    /// Inferred kind of this object based on present fields.
    pub fn kind(&self) -> ObjectKind {
        if self.image.is_some() {
            ObjectKind::Image
        } else if self.particle.is_some() {
            ObjectKind::Particle
        } else if self.text.is_some() {
            ObjectKind::Text
        } else if self.sound.is_some() {
            ObjectKind::Sound
        } else if self.light.is_some() {
            ObjectKind::Light
        } else {
            ObjectKind::Other
        }
    }

    /// Path to the image or material file if this is an image layer.
    pub fn image_path(&self) -> Option<String> {
        let val = self.image.as_ref()?;
        match val {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(map) => map
                .get("file")
                .or_else(|| map.get("image"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    pub fn alpha(&self) -> f32 {
        self.alpha.unwrap_or(1.0)
    }

    pub fn origin(&self) -> [f32; 3] {
        self.origin.map(|v| v.0).unwrap_or([0.0, 0.0, 0.0])
    }

    pub fn scale(&self) -> [f32; 3] {
        self.scale.map(|v| v.0).unwrap_or([1.0, 1.0, 1.0])
    }

    pub fn angles(&self) -> [f32; 3] {
        self.angles.map(|v| v.0).unwrap_or([0.0, 0.0, 0.0])
    }

    pub fn size(&self) -> Option<[f32; 2]> {
        self.size.map(|s| s.0)
    }
}

/// The top-level `scene.json` document.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Scene {
    #[serde(default)]
    pub camera: Option<Camera>,
    #[serde(default)]
    pub general: Option<General>,
    #[serde(default)]
    pub objects: Vec<SceneObject>,
    #[serde(default)]
    pub version: Option<u32>,
    /// Per-object keyframe animations, keyed by property name, in the same
    /// order as `objects`. Filled by a second parse pass over the raw JSON
    /// because a property carries both its base value and its animation under
    /// one key, which a plain field mapping cannot express.
    #[serde(skip)]
    pub animations: Vec<HashMap<String, crate::animation::Animation>>,
}

impl Scene {
    /// Parse a scene from a JSON string, stripping UTF-8 BOM if present.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        let json = json.strip_prefix("\u{feff}").unwrap_or(json);
        let mut scene: Scene = serde_json::from_str(json)?;
        scene.attach_animations(json);
        Ok(scene)
    }

    /// Parse a scene from a reader.
    pub fn from_reader<R: std::io::Read>(reader: R) -> Result<Self, serde_json::Error> {
        let mut buf = String::new();
        let mut reader = reader;
        std::io::Read::read_to_string(&mut reader, &mut buf).map_err(serde_json::Error::io)?;
        Self::from_json_str(&buf)
    }

    /// Collect each object's animated properties from the raw JSON.
    ///
    /// An animatable property may appear either as a bare value or as an object
    /// carrying `value` plus `animation`; this pass keeps the animation of the
    /// latter. A property that fails to parse is dropped rather than failing the
    /// whole scene — a wallpaper with one malformed animation should still render.
    fn attach_animations(&mut self, json: &str) {
        self.animations.clear();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        let Some(objects) = value.get("objects").and_then(|o| o.as_array()) else {
            return;
        };
        for (i, raw) in objects.iter().enumerate() {
            let mut map = HashMap::new();
            if i < self.objects.len() {
                if let Some(fields) = raw.as_object() {
                    for (key, field) in fields {
                        if let Some(anim) = field.get("animation") {
                            if let Ok(parsed) =
                                serde_json::from_value::<crate::animation::Animation>(
                                    anim.clone(),
                                )
                            {
                                map.insert(key.clone(), parsed);
                            }
                        }
                    }
                }
            }
            self.animations.push(map);
        }
    }

    /// Count objects by their inferred kind.
    pub fn count_by_kind(&self) -> HashMap<ObjectKind, usize> {
        let mut map = HashMap::new();
        for obj in &self.objects {
            *map.entry(obj.kind()).or_insert(0) += 1;
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vec3_string_and_array() {
        let json = r#"{"origin": "100 200.5 -50", "scale": [1.0, 2.0, 3.0], "angles": 1.5}"#;
        #[derive(Deserialize)]
        struct Test {
            origin: Vec3,
            scale: Vec3,
            angles: Vec3,
        }
        let t: Test = serde_json::from_str(json).unwrap();
        assert_eq!(t.origin, Vec3([100.0, 200.5, -50.0]));
        assert_eq!(t.scale, Vec3([1.0, 2.0, 3.0]));
        assert_eq!(t.angles, Vec3([1.5, 1.5, 1.5]));
    }

    #[test]
    fn parse_vec2_string_and_array() {
        let json = r#"{"size": "1920 1080", "depth": [0.5, 0.5]}"#;
        #[derive(Deserialize)]
        struct Test {
            size: Vec2,
            depth: Vec2,
        }
        let t: Test = serde_json::from_str(json).unwrap();
        assert_eq!(t.size, Vec2([1920.0, 1080.0]));
        assert_eq!(t.depth, Vec2([0.5, 0.5]));
    }

    #[test]
    fn parse_realistic_scene_json() {
        let json = r#"{
            "camera": {
                "center": "960 540 0",
                "eye": "960 540 1000",
                "up": "0 1 0",
                "fov": 45.0
            },
            "general": {
                "clearcolor": "0 0 0",
                "ambientcolor": "1 1 1"
            },
            "objects": [
                {
                    "id": 1,
                    "name": "Background",
                    "origin": "960 540 0",
                    "scale": "1 1 1",
                    "visible": true,
                    "image": "materials/background.json"
                },
                {
                    "id": 2,
                    "name": "Snowfall",
                    "origin": "960 1080 0",
                    "particle": {
                        "file": "particles/snow.json"
                    }
                },
                {
                    "id": 3,
                    "name": "Clock",
                    "origin": "100 100 0",
                    "text": {
                        "value": "12:00"
                    }
                }
            ],
            "version": 1
        }"#;

        let scene = Scene::from_json_str(json).expect("valid scene json");
        assert_eq!(scene.version, Some(1));
        assert_eq!(scene.objects.len(), 3);

        let bg = &scene.objects[0];
        assert_eq!(bg.name.as_deref(), Some("Background"));
        assert_eq!(bg.kind(), ObjectKind::Image);
        assert_eq!(bg.image_path().as_deref(), Some("materials/background.json"));
        assert_eq!(bg.origin(), [960.0, 540.0, 0.0]);
        assert_eq!(bg.scale(), [1.0, 1.0, 1.0]);
        assert!(bg.is_visible());

        let snow = &scene.objects[1];
        assert_eq!(snow.kind(), ObjectKind::Particle);

        let clock = &scene.objects[2];
        assert_eq!(clock.kind(), ObjectKind::Text);

        let counts = scene.count_by_kind();
        assert_eq!(counts.get(&ObjectKind::Image), Some(&1));
        assert_eq!(counts.get(&ObjectKind::Particle), Some(&1));
        assert_eq!(counts.get(&ObjectKind::Text), Some(&1));
    }

    #[test]
    fn a_property_with_value_and_animation_parses() {
        // Regression: the Vec3/Vec2 visitors used to return as soon as they saw
        // `value`, abandoning the rest of the map and making serde report a
        // "trailing comma". An animated *vector* property (origin, scale,
        // angles) is exactly that shape, so it failed to parse at all.
        let json = r#"{
            "objects": [{
                "id": 2,
                "origin": {
                    "value": "400 400 0",
                    "animation": {
                        "c0": [{"frame": 0, "value": 400}, {"frame": 60, "value": 3400}],
                        "c1": [{"frame": 0, "value": 400}, {"frame": 60, "value": 1800}],
                        "options": {"fps": 30, "length": 60, "mode": "single"}
                    }
                },
                "scale": {"user": "size", "value": "2 2 1"},
                "size": {"value": "128 128"}
            }]
        }"#;
        let scene = Scene::from_json_str(json).expect("animated vector property parses");
        let obj = &scene.objects[0];
        assert_eq!(obj.origin(), [400.0, 400.0, 0.0]);
        assert_eq!(obj.scale(), [2.0, 2.0, 1.0]);
        assert_eq!(obj.size(), Some([128.0, 128.0]));
        let anim = scene.animations[0].get("origin").expect("origin animated");
        assert_eq!(anim.sample(2.0), Some(3400.0)); // frame 60
    }

    #[test]
    fn attaches_property_animations_by_object() {
        // The `alpha` key carries both a base value and an animation; the base
        // value keeps parsing into `alpha` while the animation lands in the
        // parallel map, keyed by property, aligned with `objects`.
        let json = r#"{
            "objects": [
                {
                    "name": "Icon",
                    "alpha": {
                        "value": 0.0,
                        "animation": {
                            "c0": [{"frame": 0, "value": 0.0}, {"frame": 18, "value": 1.0}],
                            "options": {"fps": 30, "length": 18, "mode": "single"}
                        }
                    }
                },
                { "name": "Plain", "alpha": 0.5 }
            ]
        }"#;
        let scene = Scene::from_json_str(json).unwrap();
        assert_eq!(scene.animations.len(), 2);
        let anim = scene.animations[0].get("alpha").expect("alpha is animated");
        assert_eq!(anim.options.fps, 30.0);
        assert_eq!(anim.sample(0.6), Some(1.0)); // frame 18
        assert!(scene.animations[1].is_empty());
        // The base value is still available for a non-animated read.
        assert_eq!(scene.objects[1].alpha(), 0.5);
    }

    #[test]
    fn a_scene_without_animations_gets_empty_maps() {
        let json = r#"{"objects": [{"name": "A"}, {"name": "B"}]}"#;
        let scene = Scene::from_json_str(json).unwrap();
        assert_eq!(scene.animations.len(), 2);
        assert!(scene.animations.iter().all(|m| m.is_empty()));
    }

    #[test]
    fn parse_object_with_user_properties() {
        let json = r#"{
            "objects": [
                {
                    "id": 1,
                    "name": "Fog",
                    "origin": {"value": "100 200 0"},
                    "visible": {"user": "smoke", "value": true},
                    "alpha": {"user": "opacity", "value": 0.75}
                }
            ]
        }"#;
        let scene = Scene::from_json_str(json).expect("parses user properties");
        assert_eq!(scene.objects.len(), 1);
        let obj = &scene.objects[0];
        assert_eq!(obj.origin(), [100.0, 200.0, 0.0]);
        assert_eq!(obj.visible, Some(true));
        assert_eq!(obj.alpha(), 0.75);
    }
}
