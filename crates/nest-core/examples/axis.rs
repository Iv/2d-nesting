//! Diagnostics: how each part lies in its DXF. Prints the rotation (within ±45°) that makes the
//! part's bounding box narrowest, i.e. the angle by which its long axis is off the X axis as drawn.
//!
//!     cargo run --release --example axis -p nest-core -- <dir or files...>
use nest_core::dxf_in::{read_part, ImportOptions};
use std::path::PathBuf;

fn main() {
    let opts = ImportOptions { edge_tolerance: 10.0, ..Default::default() };
    let mut files: Vec<PathBuf> = Vec::new();
    for a in std::env::args().skip(1).map(PathBuf::from) {
        if a.is_dir() {
            files.extend(nest_core::manifest::dxf_files(&a).unwrap());
        } else {
            files.push(a);
        }
    }
    for f in files {
        let part = match read_part(&f, &opts) {
            Ok(p) => p,
            Err(e) => {
                println!("{}: {e:#}", f.display());
                continue;
            }
        };
        let r = &part.shape.outer;
        let height = |deg: f64| {
            let (s, c) = (-deg).to_radians().sin_cos();
            let ys = r.iter().map(|p| s * p.x + c * p.y);
            ys.clone().fold(f64::NEG_INFINITY, f64::max) - ys.fold(f64::INFINITY, f64::min)
        };
        // Long axis as drawn: the direction (within ±45°) along which the part is narrowest across.
        let axis = (-450..=450).map(|k| k as f64 / 10.0).min_by(|a, b| height(*a).total_cmp(&height(*b))).unwrap();
        let bb = part.shape.bbox();
        println!(
            "{:<12} long axis at {axis:6.1}°   width across {:6.0} mm (as drawn {:6.0})   bbox {:.0} x {:.0}",
            part.name,
            height(axis),
            height(0.0),
            bb.width(),
            bb.height()
        );
    }
}
