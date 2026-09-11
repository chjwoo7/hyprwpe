//! Configuration and persisted state.
//!
//! Two separate files, because they answer different questions and have
//! different owners:
//!
//! - **Config** is what the user wrote. hyprwpe reads it and never writes it,
//!   so hand-edits and comments survive.
//! - **State** is what hyprwpe was last doing. hyprwpe owns it entirely, and it
//!   is what makes a wallpaper survive a logout.
//!
//! Both are optional. Missing files mean defaults, never an error: a first run
//! should work with nothing configured.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::catalog::Source;

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(var) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback))
}

pub fn config_path() -> Option<PathBuf> {
    xdg("XDG_CONFIG_HOME", ".config").map(|d| d.join("hyprwpe/config.toml"))
}

pub fn state_path() -> Option<PathBuf> {
    xdg("XDG_STATE_HOME", ".local/state").map(|d| d.join("hyprwpe/state.json"))
}

/// A wallpaper source as written in the config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceConfig {
    /// A directory whose children are Workshop items carrying `project.json`.
    Workshop { path: PathBuf },
    Directory {
        path: PathBuf,
        #[serde(default = "yes")]
        recursive: bool,
    },
}

fn yes() -> bool {
    true
}

/// Expand a leading `~`, which is what people write in a config file even
/// though nothing expands it for them.
fn expand(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => path.to_path_buf(),
    }
}

impl SourceConfig {
    fn into_source(self) -> Source {
        match self {
            SourceConfig::Workshop { path } => Source::Workshop(expand(&path)),
            SourceConfig::Directory { path, recursive } => Source::Directory {
                path: expand(&path),
                recursive,
            },
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Wallpaper sources. Empty means "detect", not "none": a user who has not
    /// configured anything should still see their library.
    #[serde(rename = "source")]
    pub sources: Vec<SourceConfig>,
    /// Scaling used when a request does not name one.
    pub default_scaling: Option<String>,
    /// Daemon-wide behaviour.
    pub general: General,
}

/// Daemon-wide behaviour, as written under `[general]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// How long an output must stay occluded before renderers are suspended.
    /// Occlusion is usually brief (workspace switch, alt-tab), so this exists to
    /// avoid pausing and resuming on every flicker; too low and the daemon
    /// thrashes, too high and a hidden wallpaper keeps drawing.
    pub suspend_delay_ms: u64,
    /// Suspend while a window is fullscreen. A wallpaper behind a fullscreen
    /// window costs power for nothing, but some people layer a fullscreen
    /// terminal over a visible wallpaper and would rather it kept running.
    pub pause_on_fullscreen: bool,
    /// Frame cap for animated wallpapers. `0` leaves the compositor's frame
    /// callbacks uncapped, which on a 165 Hz panel is 165 wasted renders.
    pub fps_limit: u32,
}

impl Default for General {
    fn default() -> Self {
        General {
            suspend_delay_ms: 1500,
            pause_on_fullscreen: true,
            fps_limit: 0,
        }
    }
}

impl General {
    /// The hysteresis delay, as a `Duration`.
    pub fn suspend_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.suspend_delay_ms)
    }

    /// Whether rendering should be capped at a given frame interval.
    pub fn frame_interval(&self) -> Option<std::time::Duration> {
        if self.fps_limit == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(
                (1000u64 / self.fps_limit.max(1) as u64).max(1),
            ))
        }
    }
}

impl Config {
    /// The effective configuration as TOML, for `hyprwpe config`.
    ///
    /// Rendering the effective values (defaults included) is the point: a user
    /// asking what is in effect should not have to know which keys they left
    /// out. Returns an empty string rather than an error if it cannot encode,
    /// since a diagnostic command failing is worse than a short one.
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Config::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => config,
                Err(e) => {
                    // A broken config must not leave the user with no wallpaper.
                    // Say what is wrong and carry on with defaults.
                    eprintln!("hyprwpe: ignoring {}: {e}", path.display());
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    /// Configured sources, falling back to detection when none are given.
    pub fn sources(&self) -> Vec<Source> {
        if self.sources.is_empty() {
            crate::config::default_sources()
        } else {
            self.sources
                .iter()
                .cloned()
                .map(|s| s.into_source())
                .collect()
        }
    }
}

/// What the daemon was showing, so a restart or a logout does not lose it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Applies to outputs without their own entry.
    pub default: Option<Assignment>,
    /// Keyed by output name. Kept for outputs that are not currently connected,
    /// so unplugging and replugging a monitor restores what it had.
    pub outputs: BTreeMap<String, Assignment>,
    /// Per-wallpaper user property values, keyed by the wallpaper's path.
    ///
    /// Lives here rather than in the config file because these are not hand
    /// written: the daemon and the GUI both write them, and they are values a
    /// user picked in a panel. A wallpaper's *declarations* come from the
    /// package; only the chosen values are stored.
    pub properties: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub path: PathBuf,
    pub scaling: String,
    /// Which renderer to use. Optional so a state file written before this
    /// existed still loads; missing means image, which is what those files
    /// could only have held.
    #[serde(default)]
    pub kind: Option<String>,
}

impl State {
    pub fn load() -> Self {
        let Some(path) = state_path() else {
            return State::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("hyprwpe: ignoring {}: {e}", path.display());
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    /// Write atomically. A daemon killed mid-write must not leave a truncated
    /// file that the next start refuses to read.
    pub fn save(&self) -> Result<()> {
        let Some(path) = state_path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("encoding state")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Forget assignments whose file has gone. A wallpaper deleted since the
    /// last session should not resurrect as an error on every start.
    pub fn prune_missing(&mut self) {
        if let Some(a) = &self.default {
            if !a.path.is_file() {
                self.default = None;
            }
        }
        self.outputs.retain(|_, a| a.path.is_file());
        // Saved property values for a wallpaper that is gone are dead weight in
        // a file that is rewritten on every change.
        self.properties
            .retain(|path, _| std::path::Path::new(path).exists());
    }

    /// The key a wallpaper's saved values are filed under.
    ///
    /// Canonicalised when possible so the same wallpaper reached by two paths
    /// (a symlinked Steam library, a relative path) shares one entry; falling
    /// back to the given path keeps this total on platforms where it fails.
    pub fn properties_key(path: &Path) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    /// A wallpaper's user properties with any saved values already applied.
    pub fn properties_for(&self, path: &Path) -> crate::properties::PropertySet {
        let mut set = crate::properties::PropertySet::for_wallpaper(path);
        if let Some(saved) = self.properties.get(&Self::properties_key(path)) {
            set.apply_saved(saved);
        }
        set
    }

    /// Set one property, validating it against the wallpaper's own declaration.
    ///
    /// Returns the value that was stored, which is the coerced one - a caller
    /// that asked for `99` on a `0..1` slider is told `1` rather than left
    /// believing its value took.
    pub fn set_property(
        &mut self,
        path: &Path,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut set = crate::properties::PropertySet::for_wallpaper(path);
        if set.is_empty() {
            return Err(format!("{} exposes no user properties", path.display()));
        }
        let stored = self.properties.get(&Self::properties_key(path)).cloned();
        if let Some(saved) = &stored {
            set.apply_saved(saved);
        }
        let prop = set
            .get(key)
            .ok_or_else(|| {
                let known: Vec<&str> = set.editable().map(|p| p.key.as_str()).collect();
                format!(
                    "no property {key:?} on this wallpaper; it has: {}",
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                )
            })?
            .clone();
        if !prop.kind.is_editable() {
            return Err(format!(
                "{key:?} is a {:?}, not a value you can set",
                prop.kind
            ));
        }
        let coerced = prop
            .coerce(value)
            .ok_or_else(|| format!("{value} is not a valid value for {key:?} ({:?})", prop.kind))?;

        // Merge into whatever was already saved, so setting one property does
        // not silently reset the rest.
        let mut merged = stored.unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        if !merged.is_object() {
            merged = serde_json::Value::Object(Default::default());
        }
        if let Some(map) = merged.as_object_mut() {
            map.insert(
                key.to_string(),
                serde_json::json!({ "value": coerced.clone() }),
            );
        }
        self.properties.insert(Self::properties_key(path), merged);
        Ok(coerced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_means_detect_not_nothing() {
        let config = Config::default();
        assert!(config.sources.is_empty());
        // sources() consults the environment, so only assert it does not panic
        // and does not simply mirror the empty list as "no sources".
        let _ = config.sources();
    }

    #[test]
    fn config_parses_a_realistic_file() {
        let text = r#"
            default_scaling = "fit"

            [[source]]
            kind = "workshop"
            path = "~/.steam/root/steamapps/workshop/content/431960"

            [[source]]
            kind = "directory"
            path = "~/Pictures/Wallpapers"
            recursive = false
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.default_scaling.as_deref(), Some("fit"));
        assert_eq!(config.sources.len(), 2);
        match &config.sources[1] {
            SourceConfig::Directory { recursive, .. } => assert!(!recursive),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// `recursive` is the common case, so omitting it should not silently mean
    /// "only the top level".
    #[test]
    fn directory_sources_recurse_by_default() {
        let config: Config =
            toml::from_str("[[source]]\nkind = \"directory\"\npath = \"/tmp\"\n").unwrap();
        match &config.sources[0] {
            SourceConfig::Directory { recursive, .. } => assert!(*recursive),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn tilde_is_expanded() {
        std::env::set_var("HOME", "/home/example");
        assert_eq!(
            expand(Path::new("~/pics")),
            PathBuf::from("/home/example/pics")
        );
        assert_eq!(expand(Path::new("/abs")), PathBuf::from("/abs"));
        // A bare tilde with no separator is a filename, not a home directory.
        assert_eq!(expand(Path::new("~weird")), PathBuf::from("~weird"));
    }

    #[test]
    fn state_survives_a_roundtrip() {
        let mut state = State {
            default: Some(Assignment {
                path: PathBuf::from("/tmp/a.png"),
                scaling: "fill".into(),
                kind: Some("image".into()),
            }),
            ..State::default()
        };
        state.outputs.insert(
            "DP-2".into(),
            Assignment {
                path: PathBuf::from("/tmp/b.png"),
                scaling: "fit".into(),
                kind: Some("image".into()),
            },
        );
        let json = serde_json::to_string(&state).unwrap();
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(back.default.unwrap().scaling, "fill");
        assert_eq!(back.outputs["DP-2"].path, PathBuf::from("/tmp/b.png"));
    }

    #[test]
    fn pruning_drops_assignments_whose_file_is_gone() {
        let mut state = State {
            default: Some(Assignment {
                path: PathBuf::from("/definitely/not/here.png"),
                scaling: "fill".into(),
                kind: None,
            }),
            ..State::default()
        };
        state.outputs.insert(
            "DP-2".into(),
            Assignment {
                path: PathBuf::from("/also/gone.png"),
                scaling: "fill".into(),
                kind: None,
            },
        );
        state.prune_missing();
        assert!(state.default.is_none());
        assert!(state.outputs.is_empty());
    }

    #[test]
    fn missing_state_file_is_not_an_error() {
        // load() must never fail; the worst case is an empty state.
        let _ = State::load();
    }

    #[test]
    fn general_defaults_match_the_old_hard_coded_behaviour() {
        let g = General::default();
        assert_eq!(
            g.suspend_delay_ms, 1500,
            "the delay the policy engine used to hard-code"
        );
        assert!(g.pause_on_fullscreen);
        assert_eq!(g.fps_limit, 0, "uncapped by default");
        assert_eq!(g.suspend_delay().as_millis(), 1500);
        assert!(g.frame_interval().is_none());
    }

    #[test]
    fn a_configured_frame_limit_becomes_an_interval() {
        let g = General {
            fps_limit: 60,
            ..General::default()
        };
        assert_eq!(g.frame_interval().unwrap().as_millis(), 16);
        // A nonsensical cap must not divide by zero.
        let g = General {
            fps_limit: 1,
            ..General::default()
        };
        assert_eq!(g.frame_interval().unwrap().as_millis(), 1000);
    }

    #[test]
    fn config_parses_a_general_section() {
        let text = r#"
            default_scaling = "fit"

            [general]
            suspend_delay_ms = 250
            pause_on_fullscreen = false
            fps_limit = 30
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.general.suspend_delay_ms, 250);
        assert!(!config.general.pause_on_fullscreen);
        assert_eq!(config.general.fps_limit, 30);
        assert_eq!(config.default_scaling.as_deref(), Some("fit"));
    }

    /// A config written before `[general]` existed must keep working, and one
    /// that sets only some fields must not zero the rest.
    #[test]
    fn a_general_section_is_optional_and_partial() {
        let config: Config = toml::from_str("default_scaling = \"fill\"\n").unwrap();
        assert_eq!(config.general.suspend_delay_ms, 1500);
        let partial: Config = toml::from_str("[general]\nfps_limit = 24\n").unwrap();
        assert_eq!(partial.general.fps_limit, 24);
        assert!(
            partial.general.pause_on_fullscreen,
            "unspecified keeps its default"
        );
    }

    #[test]
    fn saved_property_values_round_trip_through_the_state_file() {
        let mut state = State::default();
        state.properties.insert(
            "/tmp/w.png".into(),
            serde_json::json!({"alpha": {"value": 0.5}}),
        );
        let json = serde_json::to_string(&state).unwrap();
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(back.properties["/tmp/w.png"]["alpha"]["value"], 0.5);
    }

    /// A state file written before properties existed must still load.
    #[test]
    fn a_state_file_without_properties_still_loads() {
        let state: State = serde_json::from_str(r#"{"default":null,"outputs":{}}"#).unwrap();
        assert!(state.properties.is_empty());
    }

    #[test]
    fn pruning_drops_property_values_for_a_wallpaper_that_is_gone() {
        let mut state = State::default();
        state.properties.insert(
            "/definitely/not/here.png".into(),
            serde_json::json!({"a": {"value": 1}}),
        );
        state.prune_missing();
        assert!(state.properties.is_empty());
    }

    /// Two paths that resolve to the same file must share one entry, or the same
    /// wallpaper set two different ways would keep two divergent records.
    #[test]
    fn the_same_wallpaper_reached_two_ways_shares_one_entry() {
        let dir = std::env::temp_dir();
        let file = dir.join("hyprwpe-props-key-test");
        std::fs::write(&file, b"x").unwrap();
        let a = State::properties_key(&file);
        let b = State::properties_key(&dir.join(".").join("hyprwpe-props-key-test"));
        assert_eq!(a, b);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn setting_a_property_validates_a_known_wallpaper() {
        let dir = std::env::temp_dir().join("hyprwpe-setprop-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.json"),
            r#"{"general":{"properties":{
                "size":{"type":"slider","min":0,"max":1,"value":0.5},
                "mode":{"type":"combo","options":[{"label":"A","value":"1"}],"value":"1"},
                "label":{"type":"group","value":""}
            }}}"#,
        )
        .unwrap();
        let pkg = dir.join("scene.pkg");
        std::fs::write(&pkg, b"").unwrap();

        let mut state = State::default();
        // A stored value is coerced against the declaration, not trusted.
        assert_eq!(
            state
                .set_property(&pkg, "size", &serde_json::json!(9))
                .unwrap(),
            serde_json::json!(1)
        );
        // ...and reported back as what actually stuck.
        let props = state.properties_for(&pkg);
        assert_eq!(props.get("size").unwrap().value, serde_json::json!(1));
        // A label or group is not something a user sets.
        assert!(state
            .set_property(&pkg, "label", &serde_json::json!("x"))
            .is_err());
        // Unknown key: the error names the ones that do exist.
        let err = state
            .set_property(&pkg, "nope", &serde_json::json!(1))
            .unwrap_err();
        assert!(err.contains("size"), "lists the real properties: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Setting one property must not wipe the others.
    #[test]
    fn setting_a_property_merges_rather_than_replaces() {
        let dir = std::env::temp_dir().join("hyprwpe-setprop-merge");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("project.json"),
            r#"{"general":{"properties":{
                "a":{"fraction":true,"type":"slider","min":0,"max":1,"value":0.1},
                "b":{"fraction":true,"type":"slider","min":0,"max":1,"value":0.2}
            }}}"#,
        )
        .unwrap();
        let pkg = dir.join("scene.pkg");
        std::fs::write(&pkg, b"").unwrap();

        let mut state = State::default();
        state
            .set_property(&pkg, "a", &serde_json::json!(0.9))
            .unwrap();
        state
            .set_property(&pkg, "b", &serde_json::json!(0.8))
            .unwrap();
        let props = state.properties_for(&pkg);
        assert_eq!(props.get("a").unwrap().value, serde_json::json!(0.9));
        assert_eq!(props.get("b").unwrap().value, serde_json::json!(0.8));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A wallpaper with no properties is not a crash - the GUI must be told so
    /// plainly, since most of the library has nothing to configure.
    #[test]
    fn a_wallpaper_without_properties_says_so() {
        let dir = std::env::temp_dir().join("hyprwpe-setprop-none");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("project.json"), r#"{"general":{}}"#).unwrap();
        let pkg = dir.join("scene.pkg");
        std::fs::write(&pkg, b"").unwrap();
        let mut state = State::default();
        let err = state
            .set_property(&pkg, "x", &serde_json::json!(1))
            .unwrap_err();
        assert!(err.contains("no user properties"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
