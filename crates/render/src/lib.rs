//! Wallpaper renderers. Every renderer here is first-party; hyprwpe ships no
//! third-party wallpaper runtime.

pub mod image_layer;
pub mod scaling;

pub use image_layer::{Target, WallpaperSpec, Wallpapers};
pub use scaling::{place, Placement, Scaling};
