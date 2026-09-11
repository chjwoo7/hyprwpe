//! Where a wallpaper's content actually lives.
//!
//! A `scene.pkg` is only half the story. Wallpaper Engine's editor lets a creator
//! reference its **built-in asset library** (particle sprites like
//! `particle/halo`, effect shader preludes like `shaders/common.h`, the fonts
//! text objects use), and those files are not copied into the package — they are
//! resolved from the engine's own installation at runtime.
//!
//! Measured on the local library: 108 of 130 particle sprite textures are
//! built-in assets, not package entries. A renderer that only reads the package
//! therefore drops most of a wallpaper's particles, and cannot compile a single
//! bundled effect shader (every one of them `#include`s `common.h`).
//!
//! So content is resolved in order:
//!
//! 1. the package itself (a creator who overrode an asset ships it),
//! 2. `assets/` of the user's Wallpaper Engine install.
//!
//! Nothing is redistributed: the second source is the user's own local copy of
//! the engine, read exactly where they installed it. When it is absent the
//! package is still used and the caller simply gets `None`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::pkg::Package;

/// Wallpaper Engine's Steam app id, which names its install and Workshop dirs.
const WALLPAPER_ENGINE_APPID: &str = "431960";
const INSTALL_DIR: &str = "wallpaper_engine";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Every Steam root worth checking, most conventional first.
///
/// A Steam library can live on any drive, in which case the user points
/// `HYPRWPE_WE_ASSETS` at it (or configures it) rather than relying on detection.
fn steam_roots() -> Vec<PathBuf> {
    let Some(home) = home() else {
        return Vec::new();
    };
    [
        ".steam/root",
        ".steam/steam",
        ".local/share/Steam",
        ".var/app/com.valvesoftware.Steam/data/Steam",
    ]
    .iter()
    .map(|p| home.join(p))
    .collect()
}

/// The `assets/` directory of an installed Wallpaper Engine, if one is found.
///
/// `HYPRWPE_WE_ASSETS` overrides detection, and may point either at the install
/// directory or straight at its `assets` folder.
pub fn assets_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("HYPRWPE_WE_ASSETS") {
        let dir = PathBuf::from(dir);
        let candidate = if dir.ends_with("assets") {
            dir
        } else {
            dir.join("assets")
        };
        return candidate.is_dir().then_some(candidate);
    }
    for root in steam_roots() {
        let install = root.join("steamapps/common").join(INSTALL_DIR);
        let candidate = install.join("assets");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    let _ = WALLPAPER_ENGINE_APPID;
    None
}

/// A wallpaper's content: its package, plus the engine's assets as a fallback.
pub struct Resources {
    package: Package,
    assets: Option<PathBuf>,
}

impl Resources {
    /// Open a package, detecting the engine's assets alongside it.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Resources {
            package: Package::open(path)
                .with_context(|| format!("opening package {}", path.display()))?,
            assets: assets_dir(),
        })
    }

    /// Open with an explicit assets directory (or none), for tests and tools.
    pub fn with_assets(path: &Path, assets: Option<PathBuf>) -> Result<Self> {
        Ok(Resources {
            package: Package::open(path)
                .with_context(|| format!("opening package {}", path.display()))?,
            assets,
        })
    }

    /// Wrap an already-parsed package. Used by tests, which build a package in
    /// memory rather than writing a file.
    pub fn from_package(package: Package, assets: Option<PathBuf>) -> Self {
        Resources { package, assets }
    }

    /// The underlying package, for name listing.
    pub fn package(&self) -> &Package {
        &self.package
    }

    pub fn has_assets(&self) -> bool {
        self.assets.is_some()
    }

    /// Read an entry: the package first, then the engine's assets.
    ///
    /// Returns owned bytes so an asset read does not leak and does not depend on
    /// the caller's borrow. Everything is read once at load time, so the copy is
    /// paid once per entry rather than per frame.
    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = self.package.get(name) {
            return Some(bytes.to_vec());
        }
        let assets = self.assets.as_ref()?;
        // Asset names are relative; refuse anything that climbs out of the tree.
        if name.split(['/', '\\']).any(|part| part == "..") {
            return None;
        }
        std::fs::read(assets.join(name)).ok()
    }

    /// Read a text entry (JSON, GLSL, …).
    pub fn get_str(&self, name: &str) -> Option<String> {
        Some(String::from_utf8_lossy(&self.get(name)?).into_owned())
    }

    /// Read a `.tex` together with its `.tex-json` sidecar.
    ///
    /// The engine's asset textures state their pixel format in the sidecar, so a
    /// decoder needs both. The sidecar lives next to the file (assets) or as a
    /// `<name>-json` entry (inside a package).
    pub fn texture(&self, name: &str) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
        let bytes = self.get(name)?;
        let sidecar = self.get(&format!("{name}-json"));
        Some((bytes, sidecar))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_name_with_a_parent_escape_is_refused() {
        let dir = std::env::temp_dir().join(format!("hyprwpe-assets-{}", std::process::id()));
        let assets = dir.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let mut f = std::fs::File::create(assets.join("ok.txt")).unwrap();
        f.write_all(b"hello").unwrap();
        drop(f);

        // A package that does not exist is fine for this check: nothing is
        // resolved from it.
        let pkg_path = dir.join("empty.pkg");
        std::fs::write(&pkg_path, {
            // Minimal valid PKGV container with no entries.
            let mut b = Vec::new();
            let version = b"PKGV0001";
            b.extend_from_slice(&(version.len() as u32).to_le_bytes());
            b.extend_from_slice(version);
            b.extend_from_slice(&0u32.to_le_bytes());
            b
        })
        .unwrap();

        let res = Resources::with_assets(&pkg_path, Some(assets.clone())).unwrap();
        assert_eq!(res.get("ok.txt").as_deref(), Some(&b"hello"[..]));
        assert!(res.get("../ok.txt").is_none());
        assert!(res.get("sub/../../ok.txt").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
