//! Audit texture decoding across a directory of `.tex` files.
//!
//! Prints one line per file: the container revision it declares and whether the
//! decoder turned it into pixels. The point is to size the gap by *class*
//! (`TEXB0003`, `TEXB0004`, …) rather than fixing textures one wallpaper at a
//! time.

use hyprwpe_core::tex::TexImage;

fn revision(bytes: &[u8]) -> String {
    match bytes.windows(8).find(|w| w.starts_with(b"TEXB")) {
        Some(w) => String::from_utf8_lossy(&w[..8]).into_owned(),
        None => "no-TEXB".to_string(),
    }
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: texaudit <dir>");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("readable dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "tex"))
        .collect();
    files.sort();

    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let rev = revision(&bytes);
        match TexImage::parse(&bytes) {
            Ok(t) => println!(
                "{name}\t{rev}\tok\t{}x{}\t{:?}",
                t.width, t.height, t.format
            ),
            Err(e) => println!("{name}\t{rev}\tfail\t{e}"),
        }
    }
}
