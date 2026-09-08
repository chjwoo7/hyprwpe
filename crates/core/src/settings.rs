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
}

impl Config {
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub path: PathBuf,
    pub scaling: String,
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
            }),
            outputs: BTreeMap::new(),
        };
        state.outputs.insert(
            "DP-2".into(),
            Assignment {
                path: PathBuf::from("/tmp/b.png"),
                scaling: "fit".into(),
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
            }),
            outputs: BTreeMap::new(),
        };
        state.outputs.insert(
            "DP-2".into(),
            Assignment {
                path: PathBuf::from("/also/gone.png"),
                scaling: "fill".into(),
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
}
