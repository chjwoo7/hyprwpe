use hyprwpe_core::assets::Resources;
use hyprwpe_core::tex::TexImage;
use std::path::Path;

fn main() {
    let p = std::env::args().nth(1).unwrap();
    let name = std::env::args().nth(2).unwrap();
    let res = Resources::open(Path::new(&p)).unwrap();
    println!("assets detected: {}", res.has_assets());
    match res.get(&name) {
        Some(b) => {
            println!("got {name}: {} bytes", b.len());
            match TexImage::parse(&b) {
                Ok(t) => println!("  parsed tex {}x{} fmt={:?}", t.width, t.height, t.format),
                Err(e) => println!("  tex parse error: {e}"),
            }
            match TexImage::parse(&b).and_then(|t| t.to_rgba_image()) {
                Ok(i) => println!("  rgba ok {:?}", i.dimensions()),
                Err(e) => println!("  rgba error: {e}"),
            }
        }
        None => println!("get({name}) = None"),
    }
}
