//! Wallpaper renderers. Every renderer here is first-party; hyprwpe ships no
//! third-party wallpaper runtime.

pub mod image_layer;
pub mod scaling;

pub use image_layer::ImageWallpaper;
pub use scaling::{place, Placement, Scaling};
