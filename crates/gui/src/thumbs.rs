//! Thumbnail loading.
//!
//! A library of a hundred wallpapers is a hundred images GTK would otherwise
//! decode at full size — hundreds of megabytes for pictures shown at 240 px.
//! Loading is scaled at decode time and spread across idle callbacks, so the
//! window is usable immediately and never holds a full-resolution frame.

use gtk4::gdk;
use gtk4::gdk_pixbuf::Pixbuf;
use std::path::Path;

/// Width thumbnails are decoded at. Tall images get a proportional height.
pub const THUMB_WIDTH: i32 = 240;

/// Decode `path` straight to thumbnail size.
///
/// Returns `None` for anything unreadable or not an image; the caller shows a
/// placeholder rather than failing, since an unreadable preview says nothing
/// about whether the wallpaper itself works.
pub fn load(path: &Path) -> Option<gdk::Texture> {
    let pixbuf = Pixbuf::from_file_at_scale(path, THUMB_WIDTH, -1, true).ok()?;
    Some(gdk::Texture::for_pixbuf(&pixbuf))
}

/// Fill `picture` with the thumbnail for `path` once the UI is idle.
///
/// Spreading the work keeps the first paint fast on a large library; the
/// callback runs once and then removes itself.
pub fn load_into(picture: &gtk4::Picture, path: &Path) {
    let picture = picture.clone();
    let path = path.to_path_buf();
    gtk4::glib::idle_add_local_once(move || {
        if let Some(texture) = load(&path) {
            picture.set_paintable(Some(&texture));
        }
    });
}
