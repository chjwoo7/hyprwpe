//! Shared model for hyprwpe: what wallpapers exist and how to read them.

pub mod animation;
pub mod assets;
pub mod catalog;
pub mod client;
pub mod config;
pub mod hyprland;
pub mod mdlv;
pub mod particle;
pub mod pkg;
pub mod protocol;
pub mod scene;
pub mod settings;
pub mod tex;

pub use catalog::{Catalog, Kind, Source, Wallpaper, WallpaperId};
pub use pkg::Package;
pub use scene::{Camera, General, ObjectKind, Scene, SceneObject, Vec2, Vec3};
pub use tex::{TexFormat, TexImage};
