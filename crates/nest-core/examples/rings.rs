//! Diagnostics: closed contours of a part, their area, bbox and share of points inside the largest one.
use nest_core::dxf_in::{contours, ImportOptions};
use nest_core::geom::{point_in_ring, signed_area, BBox};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let d = dxf::Drawing::load_file(&a[1]).unwrap();
    let opts = ImportOptions { layers: a.get(2).map(|l| l.split(',').map(String::from).collect()), ..Default::default() };
    let (mut closed, open) = contours(&d, &opts).unwrap();
    closed.sort_by(|x, y| signed_area(&y.0).abs().total_cmp(&signed_area(&x.0).abs()));
    let outer = closed[0].0.clone();
    for (r, l) in &closed {
        let b = BBox::of(r.iter().copied());
        let inside = r.iter().filter(|&&p| point_in_ring(p, &outer)).count();
        println!("closed {l:6} area {:10.0} n {:5} bbox ({:.0},{:.0})-({:.0},{:.0}) inside {}/{}", signed_area(r).abs(), r.len(), b.min.x, b.min.y, b.max.x, b.max.y, inside, r.len());
    }
    for (r, l) in &open {
        let b = BBox::of(r.iter().copied());
        let inside = r.iter().filter(|&&p| point_in_ring(p, &outer)).count();
        println!("open   {l:6} n {:5} bbox ({:.0},{:.0})-({:.0},{:.0}) inside {}/{} gap {:.2}", r.len(), b.min.x, b.min.y, b.max.x, b.max.y, inside, r.len(), r[0].dist(*r.last().unwrap()));
    }
}
