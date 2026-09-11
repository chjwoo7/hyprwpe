//! Validate the MDLV parser against a directory of `.mdl` files.
//!
//! Generality is the requirement: one parser must handle every model a creator
//! ships, across format revisions. This walks a corpus and reports the fraction
//! that parse into a usable mesh, so a rule that only fits one file is obvious.
//!
//! Usage:
//!   cargo run -p hyprwpe-core --example validate_mdlv -- <dir> [--verbose]

use hyprwpe_core::mdlv;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = match args.next() {
        Some(d) => d,
        None => {
            eprintln!("usage: validate_mdlv <dir> [--verbose]");
            std::process::exit(2);
        }
    };
    let verbose = args.any(|a| a == "--verbose");

    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "mdl").unwrap_or(false))
        .collect();
    files.sort();

    let mut ok = 0usize;
    let mut total = 0usize;
    let mut versions: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    let mut failures = Vec::new();

    for path in &files {
        total += 1;
        let bytes = std::fs::read(path).expect("read mdl");
        match mdlv::parse(&bytes) {
            Ok(m) => {
                let usable = !m.mesh.positions.is_empty() && !m.mesh.indices.is_empty();
                if usable {
                    ok += 1;
                } else {
                    failures.push(format!("{}: no usable mesh", path.display()));
                }
                *versions.entry(m.version).or_default() += 1;
                if verbose {
                    let tris = m.mesh.indices.len() / 3;
                    println!(
                        "{:<44} v{:<4} verts={:<6} tris={:<6} bones={:<4} anims={:<3} stride={}",
                        path.file_name().unwrap().to_string_lossy(),
                        m.version,
                        m.mesh.positions.len(),
                        tris,
                        m.bones.len(),
                        m.animations.len(),
                        m.mesh.stride
                    );
                }
            }
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }

    if !failures.is_empty() {
        println!("\nfailures:");
        for f in failures.iter().take(20) {
            println!("  {f}");
        }
    }
    println!("\nparsed with a usable mesh: {ok}/{total}");
    println!("revisions seen: {versions:?}");
    if ok != total {
        std::process::exit(1);
    }
}
