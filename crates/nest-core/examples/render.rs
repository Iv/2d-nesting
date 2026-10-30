//! SVG-картинка контуров детали: замкнутые — чёрным, незамкнутые — красным.
use nest_core::dxf_in::{contours, ImportOptions};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let d = dxf::Drawing::load_file(&a[1]).unwrap();
    let opts = ImportOptions { layers: a.get(3).map(|l| l.split(',').map(String::from).collect()), ..Default::default() };
    let (closed, open) = contours(&d, &opts).unwrap();
    let all: Vec<_> = closed.iter().flat_map(|(r, _)| r.iter()).chain(open.iter().flat_map(|(r, _)| r.iter())).copied().collect();
    let b = nest_core::geom::BBox::of(all);
    let k = 1600.0 / (b.width() + 20.0);
    let h = (b.height() + 20.0) * k;
    let mut s = format!("<svg xmlns='http://www.w3.org/2000/svg' width='1600' height='{h:.0}'><rect width='100%' height='100%' fill='white'/><g fill='none' stroke-width='1.5'>");
    let path = |pts: &[nest_core::geom::Pt], close: bool| {
        let mut p = String::new();
        for (i, q) in pts.iter().enumerate() { p += &format!("{}{:.2},{:.2} ", if i == 0 { "M" } else { "L" }, (q.x - b.min.x + 10.0) * k, h - (q.y - b.min.y + 10.0) * k); }
        if close { p += "Z"; } p
    };
    for (r, _) in &closed { s += &format!("<path d='{}' stroke='black'/>", path(r, true)); }
    for (r, _) in &open { s += &format!("<path d='{}' stroke='red'/>", path(r, false)); }
    s += "</g></svg>";
    std::fs::write(&a[2], s).unwrap();
    eprintln!("closed {}, open {}", closed.len(), open.len());
}
