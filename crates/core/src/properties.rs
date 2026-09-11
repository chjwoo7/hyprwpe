//! Wallpaper Engine **user properties** - the settings a creator exposes and a
//! user tunes per wallpaper.
//!
//! A scene's `project.json` (and the `scene.json` inside its package) carries a
//! `general.properties` object: a map of property key -> declaration. Those
//! declarations are what appear in the engine's wallpaper settings panel, and
//! the values a user picks are what make two installs of the same wallpaper
//! differ. Supporting them is not per-wallpaper work: the *types* are a closed
//! set of nine, and every wallpaper is a combination of them.
//!
//! Measured across the local library (`cargo run -p hyprwpe-core --example
//! validate_properties`): **597 properties over 95 items**, spanning `bool`,
//! `slider`, `color`, `combo`, `textinput`, `text`, `group`, `scenetexture` and
//! a handful with no `type` at all.
//!
//! Two rules keep this generic rather than a table of special cases:
//!
//! - **An unknown type is kept, not dropped.** A future engine version adding a
//!   type must degrade to "shown, value passed through", never to "property
//!   missing" - the same version-driven rule the binary parsers follow.
//! - **Unknown declaration fields are preserved.** Creators and the editor add
//!   keys over time (`precision`, `fraction`, `order`, …); anything not modelled
//!   is carried in `extra` so a round-trip back to disk does not lose it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The kind of a user property, as its `type` string states it.
///
/// Serialised as the raw `type` string, so a round-trip preserves a type this
/// crate does not model (`Other`) exactly as the file wrote it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PropertyKind {
    /// A checkbox.
    Bool,
    /// A number within `min..max`.
    Slider,
    /// An `"r g b"` colour triple.
    Color,
    /// A dropdown of `options`.
    Combo,
    /// A free-text field.
    TextInput,
    /// A replacement for a texture in the package.
    Texture,
    /// A file/folder/url/command the wallpaper can trigger.
    UserShortcut,
    /// A heading that groups the properties under it.
    Group,
    /// A static label.
    Text,
    /// Anything this crate does not model yet - preserved verbatim.
    Other(String),
}

impl Serialize for PropertyKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PropertyKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(PropertyKind::parse(&String::deserialize(d)?))
    }
}

impl PropertyKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "bool" => PropertyKind::Bool,
            "slider" => PropertyKind::Slider,
            "color" => PropertyKind::Color,
            "combo" => PropertyKind::Combo,
            "textinput" => PropertyKind::TextInput,
            "texture" | "scenetexture" => PropertyKind::Texture,
            "usershortcut" => PropertyKind::UserShortcut,
            "group" => PropertyKind::Group,
            "text" => PropertyKind::Text,
            other => PropertyKind::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            PropertyKind::Bool => "bool",
            PropertyKind::Slider => "slider",
            PropertyKind::Color => "color",
            PropertyKind::Combo => "combo",
            PropertyKind::TextInput => "textinput",
            PropertyKind::Texture => "texture",
            PropertyKind::UserShortcut => "usershortcut",
            PropertyKind::Group => "group",
            PropertyKind::Text => "text",
            PropertyKind::Other(s) => s,
        }
    }

    /// Whether this kind holds a value a user can set. `group`/`text` are
    /// layout only, which is why the count of *editable* properties is lower
    /// than the raw property count.
    pub fn is_editable(&self) -> bool {
        !matches!(self, PropertyKind::Group | PropertyKind::Text)
    }
}

/// One option of a `combo` property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComboOption {
    pub label: String,
    pub value: String,
}

/// A slider's bounds, defaulted the way the editor defaults them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliderRange {
    pub min: f64,
    pub max: f64,
    pub step: Option<f64>,
    pub precision: Option<u32>,
    pub fraction: bool,
}

impl Default for SliderRange {
    fn default() -> Self {
        SliderRange {
            min: 0.0,
            max: 1.0,
            step: None,
            precision: None,
            fraction: false,
        }
    }
}

/// A single user property declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    /// The `general.properties` key - the identity used in saved values and in
    /// `usershadervalues` bindings.
    pub key: String,
    pub kind: PropertyKind,
    /// The value as declared, which doubles as the default.
    pub value: serde_json::Value,
    /// Human label shown in the panel.
    pub text: String,
    /// Sort key the editor assigns; `index` breaks ties.
    pub order: Option<u32>,
    pub index: Option<u32>,
    /// Present for `slider`.
    pub range: Option<SliderRange>,
    /// Present for `combo`.
    pub options: Vec<ComboOption>,
    /// Declaration fields this crate does not model, kept so a write-back does
    /// not silently drop a newer engine's additions.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Property {
    /// The label to show, falling back to the key. Some creators leave `text`
    /// empty (the editor generates the key from it), and a blank row is worse
    /// than the key.
    pub fn label(&self) -> &str {
        if self.text.trim().is_empty() {
            &self.key
        } else {
            &self.text
        }
    }

    /// Parse one declaration.
    pub fn from_declaration(key: &str, decl: &serde_json::Value) -> Option<Property> {
        // A property is normally an object. A handful of older projects write a
        // bare value instead ("myprop": 1); accept that too rather than lose it.
        let obj = match decl {
            serde_json::Value::Object(o) => o.clone(),
            serde_json::Value::Null => return None,
            other => {
                let mut o = serde_json::Map::new();
                o.insert("value".into(), other.clone());
                o
            }
        };
        let mut extra = BTreeMap::new();
        for (k, v) in &obj {
            match k.as_str() {
                "type" | "value" | "text" | "order" | "index" | "min" | "max" | "step"
                | "precision" | "fraction" | "options" => {}
                _ => {
                    extra.insert(k.clone(), v.clone());
                }
            }
        }
        let kind = obj
            .get("type")
            .and_then(|v| v.as_str())
            .map(PropertyKind::parse)
            .unwrap_or(PropertyKind::Other(String::new()));
        let options = obj
            .get("options")
            .and_then(|o| o.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some(ComboOption {
                            label: o.get("label")?.as_str()?.to_string(),
                            value: o.get("value")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let num = |k: &str| obj.get(k).and_then(|v| v.as_f64());
        let range = if kind == PropertyKind::Slider || num("min").is_some() || num("max").is_some()
        {
            Some(SliderRange {
                min: num("min").unwrap_or(0.0),
                max: num("max").unwrap_or(1.0),
                step: num("step"),
                precision: obj
                    .get("precision")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                fraction: obj
                    .get("fraction")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        } else {
            None
        };
        Some(Property {
            key: key.to_string(),
            kind,
            value: obj.get("value").cloned().unwrap_or(serde_json::Value::Null),
            text: obj
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            order: obj.get("order").and_then(|v| v.as_u64()).map(|v| v as u32),
            index: obj.get("index").and_then(|v| v.as_u64()).map(|v| v as u32),
            range,
            options,
            extra,
        })
    }

    /// Coerce a user-supplied value into this property's own type.
    ///
    /// Coercion is what makes a setting honest: a slider outside its bounds, a
    /// combo value that is not one of the options, or a boolean written as the
    /// string `"true"` must all land on something the renderer can use, rather
    /// than being applied as-is and breaking the scene later. Returns `None`
    /// when the input cannot be represented at all.
    pub fn coerce(&self, input: &serde_json::Value) -> Option<serde_json::Value> {
        match self.kind {
            PropertyKind::Bool => match input {
                serde_json::Value::Bool(b) => Some(serde_json::Value::Bool(*b)),
                serde_json::Value::Number(n) => Some(serde_json::Value::Bool(n.as_f64()? != 0.0)),
                serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "on" | "yes" | "1" => Some(serde_json::Value::Bool(true)),
                    "false" | "off" | "no" | "0" => Some(serde_json::Value::Bool(false)),
                    _ => None,
                },
                _ => None,
            },
            PropertyKind::Slider => {
                let raw = match input {
                    serde_json::Value::Number(n) => n.as_f64()?,
                    serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
                    serde_json::Value::Bool(b) => *b as u8 as f64,
                    _ => return None,
                };
                let r = self.range.clone().unwrap_or_default();
                let (lo, hi) = if r.min <= r.max {
                    (r.min, r.max)
                } else {
                    (r.max, r.min)
                };
                let mut v = raw.clamp(lo, hi);
                // Clamp and enforce whole-number sliders, but do **not** snap to
                // `step` or round to `precision`: those are editor UI hints, and
                // applying them here would silently rewrite a stored value the
                // creator chose (the library has defaults like 0.38 and 0.25 that
                // are not on their own step boundary). The panel snaps on drag.
                if !r.fraction {
                    v = v.round();
                }
                // An integer-valued slider stays an integer. The library stores
                // `"value": 1`, not `1.0`, and writing `1.0` back would rewrite
                // the user's file on every read for no gain.
                if v.fract() == 0.0 && v.abs() < 9.0e15 {
                    return Some(serde_json::Value::Number(serde_json::Number::from(
                        v as i64,
                    )));
                }
                serde_json::Number::from_f64(v).map(serde_json::Value::Number)
            }
            PropertyKind::Color => {
                // A string that is already an engine triple is kept verbatim:
                // re-formatting it would rewrite the user's file on every read
                // for no gain ("1 1 1" must not become "1.0 1.0 1.0").
                if let serde_json::Value::String(s) = input {
                    let t = s.trim();
                    let parts: Vec<&str> = t.split_whitespace().collect();
                    if parts.len() == 3 && parts.iter().all(|p| p.parse::<f32>().is_ok()) {
                        return Some(serde_json::Value::String(t.to_string()));
                    }
                }
                let parsed = parse_color(input)?;
                Some(serde_json::Value::String(format!(
                    "{} {} {}",
                    fmt_f(parsed[0]),
                    fmt_f(parsed[1]),
                    fmt_f(parsed[2])
                )))
            }
            PropertyKind::Combo => {
                // A combo with no declared value picks its first option, so the
                // panel always has something valid to show.
                if input.is_null() {
                    return Some(
                        self.options
                            .first()
                            .map(|o| serde_json::Value::String(o.value.clone()))
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
                let s = match input {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    _ => return None,
                };
                // No options declared: there is nothing to validate against, so
                // keep the value (and its JSON type) exactly as given.
                if self.options.is_empty() {
                    return Some(input.clone());
                }
                // Already a valid option value: return the input unchanged so a
                // numeric value stays numeric.
                if self.options.iter().any(|o| o.value == s) {
                    return Some(input.clone());
                }
                // The user typed the label instead; map it to its value.
                self.options
                    .iter()
                    .find(|o| o.label.eq_ignore_ascii_case(&s))
                    .map(|o| serde_json::Value::String(o.value.clone()))
            }
            PropertyKind::TextInput | PropertyKind::Texture | PropertyKind::UserShortcut => {
                match input {
                    serde_json::Value::String(s) => Some(serde_json::Value::String(s.clone())),
                    serde_json::Value::Null => Some(serde_json::Value::String(String::new())),
                    other => Some(serde_json::Value::String(other.to_string())),
                }
            }
            // Layout-only or unknown: pass the value through untouched.
            PropertyKind::Group | PropertyKind::Text | PropertyKind::Other(_) => {
                Some(input.clone())
            }
        }
    }
}

fn fmt_f(v: f32) -> String {
    let s = format!("{v}");
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Parse a colour written as `"r g b"` (the engine's format), as a `[r,g,b]`
/// array, or as `#rrggbb`.
pub fn parse_color(input: &serde_json::Value) -> Option<[f32; 3]> {
    match input {
        serde_json::Value::String(s) => {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix('#') {
                let hex = hex.trim_start_matches('#');
                if hex.len() < 6 {
                    return None;
                }
                let f = |i: usize| {
                    u8::from_str_radix(&hex[i..i + 2], 16)
                        .ok()
                        .map(|v| v as f32 / 255.0)
                };
                return Some([f(0)?, f(2)?, f(4)?]);
            }
            let parts: Vec<f32> = s
                .split_whitespace()
                .filter_map(|p| p.parse::<f32>().ok())
                .collect();
            match parts.len() {
                3 => Some([parts[0], parts[1], parts[2]]),
                4 => Some([parts[0], parts[1], parts[2]]), // alpha is not modelled yet
                1 => Some([parts[0], parts[0], parts[0]]),
                _ => None,
            }
        }
        serde_json::Value::Array(a) => {
            let p: Vec<f32> = a
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            if p.len() >= 3 {
                Some([p[0], p[1], p[2]])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The set of user properties a wallpaper exposes, with any saved values
/// layered on top.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertySet {
    /// Ordered by the editor's `order`, then `index`, then key - so a settings
    /// panel reads in the creator's intended sequence.
    pub properties: Vec<Property>,
}

impl PropertySet {
    /// Parse `general.properties` from a project or scene document.
    pub fn from_general(general: &serde_json::Value) -> PropertySet {
        let Some(map) = general.get("properties").and_then(|p| p.as_object()) else {
            return PropertySet::default();
        };
        let mut properties: Vec<Property> = map
            .iter()
            .filter_map(|(k, v)| Property::from_declaration(k, v))
            .collect();
        properties.sort_by_key(|p| {
            (
                p.order.unwrap_or(u32::MAX),
                p.index.unwrap_or(u32::MAX),
                p.key.clone(),
            )
        });
        PropertySet { properties }
    }

    /// Parse straight from a parsed `project.json` / `scene.json` document.
    pub fn from_document(doc: &serde_json::Value) -> PropertySet {
        match doc.get("general") {
            Some(g) => Self::from_general(g),
            None => PropertySet::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    pub fn len(&self) -> usize {
        self.properties.len()
    }

    pub fn get(&self, key: &str) -> Option<&Property> {
        self.properties.iter().find(|p| p.key == key)
    }

    /// Properties a user can actually change.
    pub fn editable(&self) -> impl Iterator<Item = &Property> {
        self.properties.iter().filter(|p| p.kind.is_editable())
    }

    /// Layer saved values over the declarations.
    ///
    /// The engine stores a user's choices as `{key: {"value": v}}` (the same
    /// wrapping the declarations use), so both shapes are accepted: wrapped and
    /// bare. Keys with no matching declaration are ignored - they belong to a
    /// wallpaper that has since changed, and inventing a property for them would
    /// resurrect settings the creator removed.
    pub fn apply_saved(&mut self, saved: &serde_json::Value) {
        let Some(map) = saved.as_object() else { return };
        for (key, entry) in map {
            // Skip a nested "general" wrapper if a whole document was passed.
            if key == "general" {
                if let Some(g) = entry.get("properties") {
                    self.apply_saved(g);
                }
                continue;
            }
            let Some(prop) = self.properties.iter_mut().find(|p| p.key == *key) else {
                continue;
            };
            let raw = match entry {
                serde_json::Value::Object(o) if o.contains_key("value") => &o["value"],
                other => other,
            };
            if let Some(coerced) = prop.coerce(raw) {
                prop.value = coerced;
            }
        }
    }

    /// Serialize back to the engine's saved form: `{key: {"value": v}}`, only
    /// for editable properties.
    pub fn to_saved(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for p in self.editable() {
            let mut entry = serde_json::Map::new();
            entry.insert("value".into(), p.value.clone());
            map.insert(p.key.clone(), serde_json::Value::Object(entry));
        }
        serde_json::Value::Object(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The declaration shape the local library actually uses, taken verbatim
    /// from a scene's `project.json`.
    fn real_general() -> serde_json::Value {
        json!({
            "properties": {
                "blade_color": {"index":36,"order":136,"text":"Blades Color","type":"color","value":"1 1 1"},
                "bladessize": {"fraction":true,"index":37,"max":1,"min":0,"order":137,"precision":2,"step":0.1,"text":"Blades Size","type":"slider","value":0.5},
                "huegradient": {"index":20,"options":[{"label":"Off","value":"1"},{"label":"1","value":"2"}],"order":120,"text":"Hue Gradient","type":"combo","value":"1"},
                "audioresponsive": {"index":8,"order":108,"text":"Oni Audio Responsive on/off","type":"bool","value":true},
                "schemecolor": {"index":0,"order":100,"text":"Scheme Color","type":"color","value":"0.1 0.2 0.3"}
            }
        })
    }

    #[test]
    fn a_real_general_parses_and_orders_by_the_editors_order_key() {
        let set = PropertySet::from_general(&real_general());
        assert_eq!(set.len(), 5);
        // schemecolor (order 100) first, audioresponsive (108), huegradient (120)...
        let order: Vec<&str> = set.properties.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "schemecolor",
                "audioresponsive",
                "huegradient",
                "blade_color",
                "bladessize"
            ]
        );
        assert_eq!(set.get("bladessize").unwrap().kind, PropertyKind::Slider);
    }

    #[test]
    fn every_corpus_type_maps_to_a_kind_and_unknown_ones_survive() {
        for (t, want) in [
            ("bool", PropertyKind::Bool),
            ("slider", PropertyKind::Slider),
            ("color", PropertyKind::Color),
            ("combo", PropertyKind::Combo),
            ("textinput", PropertyKind::TextInput),
            ("scenetexture", PropertyKind::Texture),
            ("texture", PropertyKind::Texture),
            ("usershortcut", PropertyKind::UserShortcut),
            ("group", PropertyKind::Group),
            ("text", PropertyKind::Text),
        ] {
            assert_eq!(PropertyKind::parse(t), want, "{t}");
        }
        // A type from a future engine version is preserved, never dropped.
        assert_eq!(
            PropertyKind::parse("holodeck"),
            PropertyKind::Other("holodeck".into())
        );
    }

    #[test]
    fn a_slider_clamps_to_its_range_and_quantises() {
        let set = PropertySet::from_general(&real_general());
        let p = set.get("bladessize").unwrap(); // min 0 max 1 step 0.1 precision 2
        assert_eq!(p.coerce(&json!(5.0)).unwrap(), json!(1), "clamped to max");
        assert_eq!(p.coerce(&json!(-3)).unwrap(), json!(0), "clamped to min");
        // `step` is a UI hint: a stored value that is not on a step boundary is
        // kept, not rounded away.
        assert_eq!(p.coerce(&json!(0.34)).unwrap(), json!(0.34));
        assert_eq!(
            p.coerce(&json!("0.7")).unwrap(),
            json!(0.7),
            "a string number works"
        );
        assert!(p.coerce(&json!("hello")).is_none());
    }

    #[test]
    fn a_whole_number_slider_never_keeps_a_fraction() {
        let g = json!({"properties": {"n": {"type":"slider","min":0,"max":2,"fraction":false,"value":0}}});
        let set = PropertySet::from_general(&g);
        let p = set.get("n").unwrap();
        assert_eq!(p.coerce(&json!(1.6)).unwrap(), json!(2));
    }

    #[test]
    fn booleans_accept_the_spellings_users_write() {
        let set = PropertySet::from_general(&real_general());
        let p = set.get("audioresponsive").unwrap();
        assert_eq!(p.coerce(&json!("false")).unwrap(), json!(false));
        assert_eq!(p.coerce(&json!("ON")).unwrap(), json!(true));
        assert_eq!(p.coerce(&json!(0)).unwrap(), json!(false));
        assert_eq!(
            p.coerce(&json!("maybe")).unwrap_or(json!(false)),
            json!(false)
        );
    }

    #[test]
    fn a_combo_accepts_a_value_or_its_label_and_rejects_nonsense() {
        let set = PropertySet::from_general(&real_general());
        let p = set.get("huegradient").unwrap();
        assert_eq!(p.coerce(&json!("2")).unwrap(), json!("2"));
        assert_eq!(
            p.coerce(&json!("Off")).unwrap(),
            json!("1"),
            "label -> value"
        );
        assert!(p.coerce(&json!("purple")).is_none(), "not an option");
    }

    #[test]
    fn colours_parse_from_engine_triples_arrays_and_hex() {
        assert_eq!(parse_color(&json!("1 0.5 0.25")).unwrap(), [1.0, 0.5, 0.25]);
        assert_eq!(
            parse_color(&json!([0.1, 0.2, 0.3])).unwrap(),
            [0.1, 0.2, 0.3]
        );
        let hex = parse_color(&json!("#ff8000")).unwrap();
        assert!((hex[0] - 1.0).abs() < 1e-6 && (hex[1] - 0.502).abs() < 0.01);
        assert!(parse_color(&json!("not a colour")).is_none());
    }

    #[test]
    fn saved_values_layer_over_the_declared_defaults() {
        let mut set = PropertySet::from_general(&real_general());
        // The engine's saved shape.
        set.apply_saved(
            &json!({"bladessize": {"value": 0.9}, "audioresponsive": {"value": false}}),
        );
        assert_eq!(set.get("bladessize").unwrap().value, json!(0.9));
        assert_eq!(set.get("audioresponsive").unwrap().value, json!(false));
        // Untouched properties keep their default.
        assert_eq!(set.get("blade_color").unwrap().value, json!("1 1 1"));
        // A bare value (no wrapper) is accepted too.
        let mut bare = PropertySet::from_general(&real_general());
        bare.apply_saved(&json!({"blade_color": "0.5 0.5 0.5"}));
        assert_eq!(bare.get("blade_color").unwrap().value, json!("0.5 0.5 0.5"));
    }

    #[test]
    fn a_saved_value_out_of_range_is_coerced_not_trusted() {
        let mut set = PropertySet::from_general(&real_general());
        set.apply_saved(&json!({"bladessize": {"value": 99}}));
        assert_eq!(set.get("bladessize").unwrap().value, json!(1));
    }

    #[test]
    fn a_saved_key_with_no_declaration_is_ignored() {
        let mut set = PropertySet::from_general(&real_general());
        set.apply_saved(&json!({"removed_property": {"value": 1}}));
        assert_eq!(set.len(), 5, "does not resurrect a removed property");
    }

    #[test]
    fn round_trip_back_to_the_engines_saved_shape() {
        let set = PropertySet::from_general(&real_general());
        let saved = set.to_saved();
        assert_eq!(saved["bladessize"]["value"], json!(0.5));
        assert_eq!(saved["audioresponsive"]["value"], json!(true));
        // Re-applying our own output is a no-op.
        let mut again = PropertySet::from_general(&real_general());
        again.apply_saved(&saved);
        assert_eq!(again.properties, set.properties);
    }

    #[test]
    fn a_group_is_kept_but_is_not_editable() {
        let g = json!({"properties": {
            "ar": {"index":31,"order":131,"text":"AR/Media","type":"group","value":""},
            "vol": {"type":"slider","min":0,"max":1,"value":0.5}
        }});
        let set = PropertySet::from_general(&g);
        assert_eq!(set.len(), 2, "the group is still listed");
        let editable: Vec<&str> = set.editable().map(|p| p.key.as_str()).collect();
        assert_eq!(editable, vec!["vol"]);
        // A group is not written into saved values.
        assert!(set.to_saved().get("ar").is_none());
    }

    #[test]
    fn unknown_declaration_fields_survive_a_round_trip() {
        let g = json!({"properties": {
            "x": {"type":"bool","value":true,"futureKeyboardHint":"press F"}
        }});
        let set = PropertySet::from_general(&g);
        let p = set.get("x").unwrap();
        assert_eq!(p.extra.get("futureKeyboardHint").unwrap(), "press F");
    }

    #[test]
    fn a_property_with_no_type_is_still_shown() {
        let g = json!({"properties": {"mystery": {"value": 3}}});
        let set = PropertySet::from_general(&g);
        assert_eq!(set.len(), 1);
        assert_eq!(set.get("mystery").unwrap().value, json!(3));
    }

    #[test]
    fn a_blank_label_falls_back_to_the_key() {
        let g = json!({"properties": {"newproperty": {"type":"textinput","text":"","value":""}}});
        let set = PropertySet::from_general(&g);
        assert_eq!(set.get("newproperty").unwrap().label(), "newproperty");
    }

    /// Found by `validate_properties` on the real library: an integer-valued
    /// slider must not be rewritten as a float.
    #[test]
    fn an_integral_slider_keeps_its_integer_type() {
        let g = json!({"properties": {
            "soundsensitivity": {"type":"slider","min":0,"max":100,"value":100},
            "bladesopacity": {"fraction":true,"type":"slider","min":0,"max":5,"value":1}
        }});
        let set = PropertySet::from_general(&g);
        assert_eq!(
            set.get("soundsensitivity")
                .unwrap()
                .coerce(&json!(100))
                .unwrap(),
            json!(100)
        );
        assert_eq!(
            set.get("bladesopacity").unwrap().coerce(&json!(1)).unwrap(),
            json!(1)
        );
        // A fractional slider is unaffected.
        assert_eq!(
            set.get("bladesopacity")
                .unwrap()
                .coerce(&json!(0.5))
                .unwrap(),
            json!(0.5)
        );
    }

    /// Also from the library: a combo whose default is the number `0` must not
    /// be turned into the string `"0"`.
    #[test]
    fn a_numeric_combo_default_keeps_its_number_type() {
        let g = json!({"properties": {
            "instrument": {"type":"combo","options":[{"label":"Piano","value":"0"},{"label":"Drum","value":"1"}],"value":0}
        }});
        let set = PropertySet::from_general(&g);
        let p = set.get("instrument").unwrap();
        assert_eq!(p.coerce(&json!(0)).unwrap(), json!(0), "stays a number");
        assert_eq!(
            p.coerce(&json!("Drum")).unwrap(),
            json!("1"),
            "label maps to value"
        );
        assert_eq!(
            p.coerce(&json!("9")).unwrap_or(json!(null)),
            json!(null),
            "not an option"
        );
    }

    /// Every default in the library must survive its own coercion unchanged.
    #[test]
    fn coercing_a_default_is_a_no_op() {
        let set = PropertySet::from_general(&real_general());
        for p in &set.properties {
            if p.kind.is_editable() {
                assert_eq!(
                    p.coerce(&p.value).as_ref(),
                    Some(&p.value),
                    "{} {:?}",
                    p.key,
                    p.kind
                );
            }
        }
    }

    #[test]
    fn a_scene_without_properties_yields_an_empty_set_not_a_failure() {
        let set = PropertySet::from_document(&json!({"general": {"clearcolor": "0 0 0"}}));
        assert!(set.is_empty());
    }
}
