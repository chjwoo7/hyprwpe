//! Offline proof that puppet skinning is correct, with no GPU involved.
//!
//! Loads a `.mdl`, builds the rig, and prints the mesh bounding box at several
//! times. Two facts must hold on real files:
//!   * at t = 0 (frame 0, the bind pose) the deformed mesh equals the authored
//!     one — the skin is the identity, so nothing drifts;
//!   * at t > 0 the mesh actually deforms, and the motion is bounded (no NaN, no
//!     exploded vertices).
//!
//! Usage:
//!   cargo run -p hyprwpe-render --example skinpreview -- <file.mdl> [anim] [fps-step]

use hyprwpe_core::mdlv;
use hyprwpe_render::skin::{deform, Rig};

fn bbox(pts: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for p in pts {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

fn max_delta(a: &[[f32; 2]], b: &[[f32; 2]]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(p, q)| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt())
        .fold(0.0f32, f32::max)
}

/// Run every `.mdl` in a directory and check each skinning pose stays finite and
/// bounded, so a bad compose or inverse shows up as an exploded or NaN mesh.
fn corpus(dir: &str) {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "mdl").unwrap_or(false))
        .collect();
    files.sort();

    let mut checked = 0usize;
    let mut failed = 0usize;
    println!(
        "{:<50} {:>6} {:>5} {:>9} {:>9}  verdict",
        "file", "verts", "anims", "diagonal", "peak"
    );
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(model) = mdlv::parse(&bytes) else {
            continue;
        };
        if !model.mesh.is_skinned() || model.animations.is_empty() {
            continue;
        }
        checked += 1;
        let rig = Rig::new(&model);
        let mut rest = Vec::new();
        deform(&model.mesh, &rig.pose(None, 0.0), &mut rest);
        let (lo, hi) = bbox(&rest);
        let diagonal = ((hi[0] - lo[0]).hypot(hi[1] - lo[1])).max(1.0);

        let mut peak = 0.0f32;
        let mut finite = true;
        let mut buf = Vec::new();
        // Every animation, a handful of samples each.
        for anim in &model.animations {
            let duration = anim.length as f32 / anim.fps.max(1.0);
            let n = 8;
            for k in 0..=n {
                let t = duration * k as f32 / n as f32;
                deform(&model.mesh, &rig.pose(Some(anim), t), &mut buf);
                finite &= buf.iter().all(|p| p[0].is_finite() && p[1].is_finite());
                peak = peak.max(max_delta(&rest, &buf));
            }
        }
        // Deformation beyond the model's own size means a broken transform.
        let ok = finite && peak < diagonal * 4.0;
        if !ok {
            failed += 1;
        }
        println!(
            "{:<50} {:>6} {:>5} {:>9.0} {:>9.1}  {}",
            path.file_name().unwrap().to_string_lossy(),
            model.mesh.positions.len(),
            model.animations.len(),
            diagonal,
            peak,
            if !finite {
                "FAIL: non-finite"
            } else if !ok {
                "FAIL: exploded"
            } else {
                "ok"
            }
        );
    }
    println!("\nskinned models checked: {checked}, failures: {failed}");
    if failed > 0 {
        std::process::exit(1);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: skinpreview <file.mdl>|--corpus <dir> [anim] [step]");
    if path == "--corpus" {
        let dir = args.next().expect("usage: skinpreview --corpus <dir>");
        corpus(&dir);
        return;
    }
    let anim_idx: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let step: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.5);

    let bytes = std::fs::read(&path).expect("read mdl");
    let model = mdlv::parse(&bytes).expect("parse mdl");

    println!("file      : {path}");
    println!(
        "mesh      : {} verts / {} tris, stride {}, skinned {}",
        model.mesh.positions.len(),
        model.mesh.indices.len() / 3,
        model.mesh.stride,
        model.mesh.is_skinned()
    );
    println!("bones     : {}", model.bones.len());
    println!("animations: {}", model.animations.len());
    if model.animations.is_empty() {
        println!("no animation to preview");
        return;
    }

    let rig = Rig::new(&model);
    let anim = &model.animations[anim_idx.min(model.animations.len() - 1)];
    println!(
        "using anim: '{}' mode={} fps={} length={} frames",
        anim.name, anim.mode, anim.fps, anim.length
    );

    // Rest pose from the authored mesh.
    let mut rest = Vec::new();
    deform(&model.mesh, &rig.pose(None, 0.0), &mut rest);

    // t = 0 through the animation. Most models author frame 0 as the bind pose,
    // in which case the mesh is undeformed here; some start elsewhere, which is a
    // property of the model, not an error. Either way the pose must be the
    // animation's, never the authored mesh.
    let mut at0 = Vec::new();
    deform(&model.mesh, &rig.pose(Some(anim), 0.0), &mut at0);
    let drift = max_delta(&rest, &at0);
    println!(
        "\nframe-0 offset from the bind pose: {drift:.3e}  ({})",
        if drift < 1e-2 {
            "frame 0 equals the bind pose"
        } else {
            "the animation starts away from the bind pose (model data)"
        }
    );

    // Walk the animation and report how far the mesh travels.
    let duration = anim.length as f32 / anim.fps.max(1.0);
    println!(
        "\n{:<8} {:<28} {:<10} max vertex move",
        "t(s)", "bbox min", "max x-span"
    );
    let mut out = Vec::new();
    let mut t = 0.0f32;
    let mut peak = 0.0f32;
    let mut finite = true;
    while t <= duration + 1e-4 {
        deform(&model.mesh, &rig.pose(Some(anim), t), &mut out);
        let (lo, hi) = bbox(&out);
        let mv = max_delta(&rest, &out);
        peak = peak.max(mv);
        finite &= out.iter().all(|p| p[0].is_finite() && p[1].is_finite());
        println!(
            "{:<8.2} ({:<7.1},{:<7.1}) → ({:<7.1},{:<7.1}) {:>9.1}  {:>9.1}",
            t,
            lo[0],
            lo[1],
            hi[0],
            hi[1],
            hi[0] - lo[0],
            mv
        );
        t += step;
    }
    println!(
        "\npeak vertex travel: {peak:.1} model units across {} frames; all finite: {}",
        anim.length, finite
    );
}
