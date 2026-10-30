//! SVG of model-space geometry in a window, coloured by entity colour (ACI), with texts.
//!     msvg <dxf> <out.svg> x0 y0 x1 y1
use dxf::entities::EntityType;

fn aci(c: &dxf::Color) -> &'static str {
    match c.index() {
        Some(1) => "#e00", Some(2) => "#cc0", Some(3) => "#0a0", Some(4) => "#0aa", Some(5) => "#00e", Some(6) => "#c0c",
        _ => "#333",
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (x0, y0, x1, y1): (f64, f64, f64, f64) = (a[3].parse().unwrap(), a[4].parse().unwrap(), a[5].parse().unwrap(), a[6].parse().unwrap());
    let d = dxf::Drawing::load_file(&a[1]).unwrap();
    let k = 1800.0 / (x1 - x0);
    let h = (y1 - y0) * k;
    let m = |x: f64, y: f64| format!("{:.1},{:.1}", (x - x0) * k, h - (y - y0) * k);
    let inside = |x: f64, y: f64| x >= x0 && x <= x1 && y >= y0 && y <= y1;
    let mut s = format!("<svg xmlns='http://www.w3.org/2000/svg' width='1800' height='{h:.0}' viewBox='0 0 1800 {h:.0}'><rect width='100%' height='100%' fill='#fff'/>");
    for e in d.entities() {
        let col = aci(&e.common.color);
        match &e.specific {
            EntityType::Line(l) if inside(l.p1.x, l.p1.y) => s += &format!("<path d='M{} L{}' stroke='{col}' stroke-width='0.5'/>", m(l.p1.x, l.p1.y), m(l.p2.x, l.p2.y)),
            EntityType::LwPolyline(p) if p.vertices.first().is_some_and(|v| inside(v.x, v.y)) => {
                let pts: Vec<String> = p.vertices.iter().map(|v| m(v.x, v.y)).collect();
                s += &format!("<path d='M{}{}' fill='none' stroke='{col}' stroke-width='1'/>", pts.join(" L"), if p.flags & 1 != 0 { " Z" } else { "" });
            }
            EntityType::Text(t) if inside(t.location.x, t.location.y) => s += &format!("<text x='{}' y='{}' font-size='10' fill='#00f'>{}</text>", (t.location.x - x0) * k, h - (t.location.y - y0) * k, t.value),
            EntityType::MText(t) if inside(t.insertion_point.x, t.insertion_point.y) => s += &format!("<text x='{}' y='{}' font-size='12' fill='#f0f'>{}</text>", (t.insertion_point.x - x0) * k, h - (t.insertion_point.y - y0) * k, t.text),
            _ => {}
        }
    }
    s += "</svg>";
    std::fs::write(&a[2], s).unwrap();
}
