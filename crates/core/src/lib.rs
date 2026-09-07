//! Shared model for hyprwpe: what wallpapers exist and how to read them.

pub mod catalog;
pub mod config;
pub mod pkg;
pub mod protocol;

pub use catalog::{Catalog, Kind, Source, Wallpaper, WallpaperId};
pub use pkg::Package;
