//! Wallpaper renderers. Every renderer here is first-party; hyprwpe ships no
//! third-party wallpaper runtime.

pub mod gl;
pub mod image_layer;
pub mod mpv_dl;
pub mod scaling;
pub mod scene_layer;
pub mod shader_layer;
pub mod video_layer;

pub use image_layer::{Target, WallpaperSpec, Wallpapers};
pub use scaling::{place, Placement, Scaling};
pub use scene_layer::ScenePlayer;
pub use shader_layer::ShaderPlayer;
