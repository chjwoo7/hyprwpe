//! What hyprwpe can display, gathered from configured sources.
//!
//! A wallpaper is anything a source can describe: a Steam Workshop item, or an
//! ordinary file on disk. Wallpaper Engine is one source among several, not the
//! centre of the model.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Extensions treated as still images. Matches the set the end4 shell family
/// already accepts, so a wallpaper visible in its picker is visible here too.
pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "avif", "bmp", "svg", "gif"];
pub const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mkv", "avi", "mov"];
pub const SHADER_EXTS: &[&str] = &["glsl", "frag"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Shader,
    /// Wallpaper Engine scene package.
    Scene,
    /// Wallpaper Engine HTML wallpaper. Recognised so it can be reported, but
    /// deferred: rendering it needs a browser engine.
    Web,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Shader => "shader",
            Kind::Scene => "scene",
            Kind::Web => "web",
        }
    }

    fn from_extension(path: &Path) -> Option<Kind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        let has = |set: &[&str]| set.contains(&ext.as_str());
        if has(IMAGE_EXTS) {
            Some(Kind::Image)
        } else if has(VIDEO_EXTS) {
            Some(Kind::Video)
        } else if has(SHADER_EXTS) {
            Some(Kind::Shader)
        } else {
            None
        }
    }

    /// Wallpaper Engine's own `type` field, which is capitalised inconsistently
    /// across Workshop items.
    fn from_project_type(ty: &str) -> Option<Kind> {
        match ty.to_ascii_lowercase().as_str() {
            "scene" => Some(Kind::Scene),
            "video" => Some(Kind::Video),
            "web" => Some(Kind::Web),
            _ => None,
        }
    }
}

/// How a wallpaper is addressed. A Workshop item is identified by its id; any
/// other wallpaper is identified by its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperId {
    Wpe(String),
    File(PathBuf),
}

impl std::fmt::Display for WallpaperId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WallpaperId::Wpe(id) => f.write_str(id),
            WallpaperId::File(p) => write!(f, "{}", p.display()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Wallpaper {
    pub id: WallpaperId,
    pub title: String,
    pub kind: Kind,
    /// Directory for a Workshop item, file for anything else.
    pub path: PathBuf,
    /// Still image representing the wallpaper, when one exists.
    pub preview: Option<PathBuf>,
}

impl Wallpaper {
    /// Whether hyprwpe can render this today. Reported rather than hidden: a
    /// wallpaper the user owns should appear in listings even when unsupported,
    /// with the reason visible.
    pub fn supported(&self) -> bool {
        self.kind != Kind::Web
    }
}

#[derive(Debug, Clone)]
pub enum Source {
    /// A directory whose children are Workshop items carrying `project.json`.
    /// Steam does not need to be installed for this to work.
    Workshop(PathBuf),
    Directory {
        path: PathBuf,
        recursive: bool,
    },
}

/// The subset of `project.json` hyprwpe reads.
#[derive(Debug, Deserialize)]
struct Project {
    title: Option<String>,
    #[serde(rename = "type")]
    ty: Option<String>,
    preview: Option<String>,
}

#[derive(Debug, Default)]
pub struct Catalog {
    pub wallpapers: Vec<Wallpaper>,
    /// Entries that could not be read, with the reason. Surfaced rather than
    /// swallowed: a wallpaper silently missing from a listing is the kind of
    /// failure this project exists to avoid.
    pub problems: Vec<(PathBuf, String)>,
}

impl Catalog {
    pub fn scan(sources: &[Source]) -> Self {
        let mut cat = Catalog::default();
        for source in sources {
            match source {
                Source::Workshop(root) => cat.scan_workshop(root),
                Source::Directory { path, recursive } => cat.scan_directory(path, *recursive),
            }
        }
        cat.wallpapers.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.title.cmp(&b.title))
        });
        cat
    }

    fn scan_workshop(&mut self, root: &Path) {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(e) => {
                self.problems.push((root.to_path_buf(), e.to_string()));
                return;
            }
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            if !dir.join("project.json").exists() {
                continue;
            }
            match Self::read_workshop_item(&dir) {
                Ok(w) => self.wallpapers.push(w),
                Err(e) => self.problems.push((dir, format!("{e:#}"))),
            }
        }
    }

    fn read_workshop_item(dir: &Path) -> Result<Wallpaper> {
        let raw =
            std::fs::read_to_string(dir.join("project.json")).context("reading project.json")?;
        let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
        let project: Project = serde_json::from_str(raw).context("parsing project.json")?;

        let id = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        let kind = project
            .ty
            .as_deref()
            .and_then(Kind::from_project_type)
            // Some items omit or misspell `type`; a scene package is proof enough.
            .or_else(|| dir.join("scene.pkg").exists().then_some(Kind::Scene))
            .with_context(|| {
                format!(
                    "unknown wallpaper type {:?}",
                    project.ty.as_deref().unwrap_or("<missing>")
                )
            })?;

        let preview = project.preview.map(|p| dir.join(p)).filter(|p| p.exists());

        Ok(Wallpaper {
            title: project.title.unwrap_or_else(|| id.clone()),
            id: WallpaperId::Wpe(id),
            kind,
            path: dir.to_path_buf(),
            preview,
        })
    }

    fn scan_directory(&mut self, dir: &Path, recursive: bool) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                self.problems.push((dir.to_path_buf(), e.to_string()));
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if recursive {
                    self.scan_directory(&path, true);
                }
                continue;
            }
            let Some(kind) = Kind::from_extension(&path) else {
                continue;
            };
            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string();
            let preview = matches!(kind, Kind::Image).then(|| path.clone());
            self.wallpapers.push(Wallpaper {
                id: WallpaperId::File(path.clone()),
                title,
                kind,
                path,
                preview,
            });
        }
    }

    pub fn count_by_kind(&self) -> Vec<(Kind, usize)> {
        let kinds = [
            Kind::Image,
            Kind::Video,
            Kind::Shader,
            Kind::Scene,
            Kind::Web,
        ];
        kinds
            .into_iter()
            .map(|k| (k, self.wallpapers.iter().filter(|w| w.kind == k).count()))
            .filter(|(_, n)| *n > 0)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_extension_is_case_insensitive() {
        assert_eq!(Kind::from_extension(Path::new("a.PNG")), Some(Kind::Image));
        assert_eq!(Kind::from_extension(Path::new("a.Mp4")), Some(Kind::Video));
        assert_eq!(Kind::from_extension(Path::new("a.txt")), None);
        assert_eq!(Kind::from_extension(Path::new("noext")), None);
    }

    #[test]
    fn project_type_casing_varies_in_the_wild() {
        assert_eq!(Kind::from_project_type("scene"), Some(Kind::Scene));
        assert_eq!(Kind::from_project_type("Scene"), Some(Kind::Scene));
        assert_eq!(Kind::from_project_type("Web"), Some(Kind::Web));
        assert_eq!(Kind::from_project_type("nonsense"), None);
    }

    #[test]
    fn web_is_listed_but_unsupported() {
        let w = Wallpaper {
            id: WallpaperId::Wpe("1".into()),
            title: "t".into(),
            kind: Kind::Web,
            path: PathBuf::new(),
            preview: None,
        };
        assert!(!w.supported());
    }
}
