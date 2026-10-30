//! Model-space geometry (not inserts): counts by layer/colour, extents, and texts with positions.
use dxf::entities::EntityType;
use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("path to a DXF");
    let d = dxf::Drawing::load_file(&path).unwrap();
    let mut groups: BTreeMap<(String, String, String), (usize, f64, f64, f64, f64)> = BTreeMap::new();
    for e in d.entities() {
        let pts: Vec<(f64, f64)> = match &e.specific {
            EntityType::Line(l) => vec![(l.p1.x, l.p1.y), (l.p2.x, l.p2.y)],
            EntityType::LwPolyline(p) => p.vertices.iter().map(|v| (v.x, v.y)).collect(),
            EntityType::Circle(c) => vec![(c.center.x, c.center.y)],
            EntityType::Arc(a) => vec![(a.center.x, a.center.y)],
            EntityType::Text(t) => {
                println!("TEXT {:8} col {:?} at ({:.0},{:.0}) h {:.0} {:?}", e.common.layer, e.common.color, t.location.x, t.location.y, t.text_height, t.value);
                continue;
            }
            EntityType::MText(t) => {
                println!("MTEXT {:8} col {:?} at ({:.0},{:.0}) {:?}", e.common.layer, e.common.color, t.insertion_point.x, t.insertion_point.y, t.text);
                continue;
            }
            _ => continue,
        };
        let kind = format!("{:?}", e.specific).split('(').next().unwrap().to_string();
        let g = groups.entry((e.common.layer.clone(), format!("{:?}", e.common.color), kind)).or_insert((0, f64::MAX, f64::MAX, f64::MIN, f64::MIN));
        g.0 += 1;
        for (x, y) in pts { g.1 = g.1.min(x); g.2 = g.2.min(y); g.3 = g.3.max(x); g.4 = g.4.max(y); }
    }
    for ((l, c, k), (n, x0, y0, x1, y1)) in groups {
        println!("GEOM {l:8} {c:24} {k:12} n {n:5} x {x0:.0}..{x1:.0} y {y0:.0}..{y1:.0}");
    }
}
