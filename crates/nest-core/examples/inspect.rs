//! DXF structure: model-space entities by layer and type, list of blocks with sizes and texts.
use dxf::entities::EntityType;
use dxf::Drawing;
use std::collections::BTreeMap;

fn kind(e: &EntityType) -> String {
    format!("{e:?}").split('(').next().unwrap().to_string()
}

fn main() {
    let path = std::env::args().nth(1).expect("path to a DXF");
    let d = Drawing::load_file(&path).unwrap();
    println!("units {:?}, version {:?}", d.header.default_drawing_units, d.header.version);
    let mut ms: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut inserts: BTreeMap<String, (usize, String, f64, f64, f64)> = BTreeMap::new();
    for e in d.entities() {
        *ms.entry((e.common.layer.clone(), kind(&e.specific))).or_default() += 1;
        if let EntityType::Insert(i) = &e.specific {
            let v = inserts.entry(i.name.clone()).or_insert((0, e.common.layer.clone(), i.location.x, i.location.y, i.rotation));
            v.0 += 1;
        }
    }
    println!("== model space");
    for ((l, k), n) in &ms { println!("  {l:10} {k:12} {n}"); }
    println!("== {} distinct inserted blocks", inserts.len());
    for (name, (n, layer, x, y, rot)) in &inserts {
        let b = d.blocks().find(|b| &b.name == name).unwrap();
        let mut c: BTreeMap<String, usize> = BTreeMap::new();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for e in &b.entities {
            *c.entry(format!("{}:{}", e.common.layer, kind(&e.specific))).or_default() += 1;
            if let EntityType::LwPolyline(p) = &e.specific {
                if e.common.layer != "mark" { for v in &p.vertices { x0=x0.min(v.x); y0=y0.min(v.y); x1=x1.max(v.x); y1=y1.max(v.y);} }
            }
        }
        let texts: Vec<String> = b.entities.iter().filter_map(|e| match &e.specific {
            EntityType::Text(t) => Some(t.value.clone()), EntityType::MText(t) => Some(t.text.clone()), _ => None }).take(3).collect();
        println!("{name:>24} x{n} ins_layer={layer} at=({x:.0},{y:.0}) rot={rot:.0} bbox={:.0}x{:.0} {:?} {:?}", x1-x0, y1-y0, c, texts);
    }
}
