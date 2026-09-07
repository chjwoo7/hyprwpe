//! Where hyprwpe looks for wallpapers.
//!
//! Every path is user-overridable; the Steam location is a default that is
//! detected, not an assumption. Steam libraries move between drives and a user
//! may keep Workshop content anywhere.

use crate::catalog::Source;
use std::path::PathBuf;

/// Wallpaper Engine's Steam app id, which names its Workshop content directory.
const WALLPAPER_ENGINE_APPID: &str = "431960";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Steam roots worth checking, most conventional first. A user with a library on
/// another drive overrides this in config rather than relying on detection.
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

/// The Workshop content directory, if one exists.
pub fn detect_workshop_dir() -> Option<PathBuf> {
    steam_roots()
        .into_iter()
        .map(|root| {
            root.join("steamapps/workshop/content")
                .join(WALLPAPER_ENGINE_APPID)
        })
        .find(|p| p.is_dir())
}

/// Picture directories worth scanning when the user has not configured any.
fn default_picture_dirs() -> Vec<PathBuf> {
    let Some(home) = home() else {
        return Vec::new();
    };
    ["Pictures/Wallpapers", "Pictures"]
        .iter()
        .map(|p| home.join(p))
        .filter(|p| p.is_dir())
        .collect()
}

/// Sources to use when the user has not configured any. Deliberately quiet
/// about what it finds: absent directories are simply not returned.
pub fn default_sources() -> Vec<Source> {
    let mut sources = Vec::new();
    if let Some(dir) = detect_workshop_dir() {
        sources.push(Source::Workshop(dir));
    }
    for path in default_picture_dirs() {
        sources.push(Source::Directory {
            path,
            recursive: true,
        });
    }
    sources
}
