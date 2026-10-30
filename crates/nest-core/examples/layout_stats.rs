//! Stats of an existing (hand-made) layout DXF: top-level closed contours, their area and the bbox.
use nest_core::dxf_in::{contours, ImportOptions};
use nest_core::geom::{point_in_ring, signed_area, BBox};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let opts = ImportOptions { layers: a.get(2).map(|l| l.split(',').map(String::from).collect()), ..Default::default() };
    for path in a[1].split(';') {
        let d = dxf::Drawing::load_file(path).unwrap();
        let (mut closed, open) = contours(&d, &opts).unwrap();
        closed.sort_by(|x, y| signed_area(&y.0).abs().total_cmp(&signed_area(&x.0).abs()));
        let mut tops: Vec<&Vec<_>> = Vec::new();
        for (r, _) in &closed {
            if !tops.iter().any(|t| point_in_ring(r[0], t)) {
                tops.push(r);
            }
        }
        let b = BBox::of(closed.iter().flat_map(|(r, _)| r.iter().copied()));
        let area: f64 = tops.iter().map(|r| signed_area(r).abs()).sum();
        println!("{path}\n  top-level contours {}, closed {}, open {}, bbox {:.0} x {:.0} mm, parts area {:.3} m2, density in bbox {:.1}%",
            tops.len(), closed.len(), open.len(), b.width(), b.height(), area / 1e6, area / (b.width() * b.height()) * 100.0);
        for t in tops.iter().take(3) { let bb = BBox::of(t.iter().copied()); println!("    top {:.0}x{:.0}", bb.width(), bb.height()); }
    }
}
