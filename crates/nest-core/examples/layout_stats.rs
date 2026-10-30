//! Density of an existing (hand-made) layout DXF: parts area by outer contour over
//! sheet width × used length, the same measure as the nesting report.
//!
//!     layout_stats <sheet width> <layers or "-"> <file.dxf>...
use nest_core::dxf_in::{closed_contours, ImportOptions};
use nest_core::geom::{point_in_ring, signed_area, BBox};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let width: f64 = a[1].parse().unwrap();
    let layers = (a[2] != "-").then(|| a[2].split(',').map(String::from).collect());
    let opts = ImportOptions { layers, close_gaps: 40.0, ..Default::default() };
    let (mut total_area, mut total_len) = (0.0, 0.0);
    for path in &a[3..] {
        let d = dxf::Drawing::load_file(path).unwrap();
        let mut rings = closed_contours(&d, &opts).unwrap();
        rings.sort_by(|x, y| signed_area(y).abs().total_cmp(&signed_area(x).abs()));
        // Top-level contours are the parts; anything inside a larger ring belongs to that part.
        let mut tops: Vec<&Vec<_>> = Vec::new();
        for r in &rings {
            if !tops.iter().any(|t| point_in_ring(r[0], t)) {
                tops.push(r);
            }
        }
        let b = BBox::of(tops.iter().flat_map(|r| r.iter().copied()));
        let along = b.width().max(b.height());
        let across = b.width().min(b.height());
        let area: f64 = tops.iter().map(|r| signed_area(r).abs()).sum();
        total_area += area;
        total_len += along;
        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        println!("{name:32} parts {:3}  {:6.0} x {:4.0} mm  area {:6.3} m²  density {:5.1}%",
            tops.len(), along, across, area / 1e6, area / (width * along) * 100.0);
    }
    println!("{:32} total length {:.3} m, area {:.3} m², density {:.1}%", "ALL", total_len / 1000.0, total_area / 1e6, total_area / (width * total_len) * 100.0);
}
