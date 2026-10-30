//! Writes a sheet to DXF: layer SHEET (sheet rectangle) and PARTS (one INSERT per placed part).
//!
//! Every part is written as a block holding all entities of its source DXF, on all layers and
//! unchanged (marks, texts, arcs, …), so nothing of the drawing is lost or approximated. Block
//! entities on layer "0" take the PARTS layer of the insert; the others keep their own layers,
//! whose colours, line types and text styles are copied from the sources.

use crate::geom::{point_in_ring, Demand, Pt, Ring, Shape, Sheet};
use anyhow::{Context, Result};
use dxf::entities::{Entity, EntityType, Insert, LwPolyline, Text};
use dxf::LwPolylineVertex;
use dxf::enums::{AcadVersion, HorizontalTextJustification, Units, VerticalTextJustification};
use dxf::tables::{Layer, LineType, Style};
use dxf::{Block, Color, Drawing, Point};
use geo::InteriorPoint;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const LAYER_SHEET: &str = "SHEET";
pub const LAYER_PARTS: &str = "PARTS";
pub const LAYER_LABELS: &str = "LABELS";

/// Source geometry of every part, ready to be written as blocks.
pub struct PartLibrary {
    /// Per demand: block name, its blocks (the part plus nested ones) and the unit scale to mm.
    parts: Vec<(String, Vec<Block>, f64)>,
    layers: BTreeMap<String, Layer>,
    line_types: BTreeMap<String, LineType>,
    styles: BTreeMap<String, Style>,
}

impl PartLibrary {
    /// Loads the source DXF of every part.
    pub fn load(demands: &[Demand]) -> Result<Self> {
        let mut lib = PartLibrary { parts: Vec::new(), layers: BTreeMap::new(), line_types: BTreeMap::new(), styles: BTreeMap::new() };
        let mut cache: HashMap<&Path, Drawing> = HashMap::new();
        for (i, d) in demands.iter().enumerate() {
            let src = d.part.source.as_path();
            if !cache.contains_key(src) {
                let dr = Drawing::load_file(src).with_context(|| format!("{}: cannot read DXF", src.display()))?;
                cache.insert(src, dr);
            }
            let dr = &cache[src];
            // With duplicate layer entries the last one wins: older files written by `nest extract`
            // carry a default copy before the real one.
            let mut own: BTreeMap<String, Layer> = BTreeMap::new();
            for l in dr.layers() {
                own.insert(l.name.clone(), Layer { name: l.name.clone(), color: l.color.clone(), line_type_name: l.line_type_name.clone(), ..Default::default() });
            }
            for (n, l) in own {
                lib.layers.entry(n).or_insert(l);
            }
            for t in dr.line_types() {
                lib.line_types.entry(t.name.clone()).or_insert_with(|| LineType { handle: dxf::Handle::empty(), ..t.clone() });
            }
            for st in dr.styles() {
                lib.styles.entry(st.name.clone()).or_insert_with(|| Style { handle: dxf::Handle::empty(), ..st.clone() });
            }
            // Block names must be unique across parts; nested blocks get the part's prefix too.
            let name = format!("P{i}_{}", sanitize(&d.part.name));
            let rename = |n: &str| format!("{name}__{}", sanitize(n));
            let fix = |e: &Entity| -> Entity {
                let mut e = e.clone();
                e.common.handle = dxf::Handle::empty();
                if let EntityType::Insert(ins) = &mut e.specific {
                    ins.name = rename(&ins.name);
                }
                e
            };
            let mut blocks = Vec::new();
            for b in dr.blocks().filter(|b| !b.name.starts_with('*')) {
                let mut nb = Block { name: rename(&b.name), base_point: b.base_point.clone(), ..Default::default() };
                nb.entities = b.entities.iter().map(fix).collect();
                blocks.push(nb);
            }
            let mut main = Block { name: name.clone(), ..Default::default() };
            main.entities = dr.entities().filter(|e| !e.common.is_in_paper_space).map(fix).collect();
            blocks.push(main);
            let scale = crate::dxf_in::unit_scale(dr.header.default_drawing_units, &mut Vec::new())?;
            lib.parts.push((name, blocks, scale));
        }
        Ok(lib)
    }
}

/// Output options.
#[derive(Debug, Clone, Copy, Default)]
pub struct DxfOptions {
    /// Add the part name as text on layer LABELS.
    pub labels: bool,
}

pub fn write_sheet(path: &Path, demands: &[Demand], lib: &PartLibrary, sheet: &Sheet, opts: DxfOptions) -> Result<()> {
    let mut d = Drawing::new();
    d.header.version = AcadVersion::R2000;
    d.header.default_drawing_units = Units::Millimeters;
    for t in lib.line_types.values() {
        if !d.line_types().any(|x| x.name.eq_ignore_ascii_case(&t.name)) {
            d.add_line_type(t.clone());
        }
    }
    for st in lib.styles.values() {
        if !d.styles().any(|x| x.name.eq_ignore_ascii_case(&st.name)) {
            d.add_style(st.clone());
        }
    }
    for l in lib.layers.values() {
        if !d.layers().any(|x| x.name.eq_ignore_ascii_case(&l.name)) {
            d.add_layer(l.clone());
        }
    }
    for (name, color) in [(LAYER_SHEET, 8u8), (LAYER_PARTS, 7), (LAYER_LABELS, 3)] {
        if name != LAYER_LABELS || opts.labels {
            d.add_layer(Layer { name: name.into(), color: Color::from_index(color), ..Default::default() });
        }
    }

    let rect = vec![
        Pt::new(0.0, 0.0),
        Pt::new(sheet.length, 0.0),
        Pt::new(sheet.length, sheet.width),
        Pt::new(0.0, sheet.width),
    ];
    d.add_entity(polyline(&rect, LAYER_SHEET));

    let mut used: Vec<bool> = vec![false; lib.parts.len()];
    for p in &sheet.placements {
        if !used[p.part] {
            used[p.part] = true;
            for b in &lib.parts[p.part].1 {
                d.add_block(b.clone());
            }
        }
        let (name, _, scale) = &lib.parts[p.part];
        // Sheet point = R·(scale·p) + t: exactly what an INSERT with these values does.
        let ins = Insert {
            name: name.clone(),
            location: Point::new(p.transform.translation.x, p.transform.translation.y, 0.0),
            rotation: p.transform.rotation_deg,
            x_scale_factor: *scale,
            y_scale_factor: *scale,
            z_scale_factor: *scale,
            ..Default::default()
        };
        let mut e = Entity::new(EntityType::Insert(ins));
        e.common.layer = LAYER_PARTS.into();
        d.add_entity(e);
        if opts.labels {
            let part = &demands[p.part].part;
            d.add_entity(label(&part.name, &part.shape.transformed(&p.transform)));
        }
    }

    d.save_file(path).with_context(|| format!("{}: cannot write DXF", path.display()))
}

/// Block name -> safe DXF name.
fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || "-_".contains(c) { c } else { '_' }).collect()
}

fn polyline(ring: &Ring, layer: &str) -> Entity {
    let mut pl = LwPolyline::default();
    pl.set_is_closed(true);
    pl.vertices = ring.iter().map(|p| LwPolylineVertex { x: p.x, y: p.y, ..Default::default() }).collect();
    let mut e = Entity::new(EntityType::LwPolyline(pl));
    e.common.layer = layer.into();
    e
}

fn label(name: &str, s: &Shape) -> Entity {
    let b = s.bbox();
    let inside = |p: Pt| point_in_ring(p, &s.outer) && !s.holes.iter().any(|h| point_in_ring(p, h));
    let center = Some(b.center()).filter(|&c| inside(c)).unwrap_or_else(|| {
        let poly = geo::Polygon::new(
            s.outer.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>().into(),
            s.holes.iter().map(|h| h.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>().into()).collect(),
        );
        poly.interior_point().map(|p| Pt::new(p.x(), p.y())).unwrap_or(b.center())
    });
    // Text height chosen so the name roughly fits inside the part.
    let chars = name.chars().count().max(1) as f64;
    let height = (b.width() / (chars * 0.8)).min(b.height() * 0.25).clamp(2.0, 40.0);
    let at = Point::new(center.x, center.y, 0.0);
    let text = Text {
        location: at.clone(),
        second_alignment_point: at,
        text_height: height,
        value: name.into(),
        horizontal_text_justification: HorizontalTextJustification::Center,
        vertical_text_justification: VerticalTextJustification::Middle,
        ..Default::default()
    };
    let mut e = Entity::new(EntityType::Text(text));
    e.common.layer = LAYER_LABELS.into();
    e
}

/// Writes an SVG preview of a sheet: sheet outline, parts (outer contour filled), inner geometry and names.
pub fn write_svg(path: &Path, demands: &[Demand], sheet: &Sheet) -> Result<()> {
    write_svg_with_limit(path, demands, sheet, None)
}

/// Like [`write_svg`]; `limit` draws a red line at that length (the sheet end on a longer strip).
pub fn write_svg_with_limit(path: &Path, demands: &[Demand], sheet: &Sheet, limit: Option<f64>) -> Result<()> {
    use std::fmt::Write as _;
    // Fit the long side to 2400 px; DXF Y goes up, SVG Y goes down.
    let k = 2400.0 / sheet.length.max(sheet.width).max(1.0);
    let (w, h) = (sheet.length * k, sheet.width * k);
    let pt = |p: &Pt| format!("{:.1},{:.1}", p.x * k, h - p.y * k);
    let path_d = |r: &[Pt], close: bool| {
        let mut d = String::new();
        for (i, p) in r.iter().enumerate() {
            let _ = write!(d, "{}{} ", if i == 0 { "M" } else { "L" }, pt(p));
        }
        if close {
            d.push('Z');
        }
        d
    };
    let mut s = String::new();
    let _ = write!(
        s,
        "<svg xmlns='http://www.w3.org/2000/svg' width='{w:.0}' height='{h:.0}' viewBox='0 0 {w:.0} {h:.0}'>\
         <rect width='100%' height='100%' fill='#fff' stroke='#888' stroke-width='2'/>"
    );
    for p in &sheet.placements {
        let part = &demands[p.part].part;
        let sh = part.shape.transformed(&p.transform);
        let _ = write!(s, "<path d='{}' fill='#cfe3f7' stroke='#1f5fa8' stroke-width='1'/>", path_d(&sh.outer, true));
        for hole in &sh.holes {
            let _ = write!(s, "<path d='{}' fill='none' stroke='#1f5fa8' stroke-width='0.6'/>", path_d(hole, true));
        }
        for l in &sh.lines {
            let _ = write!(s, "<path d='{}' fill='none' stroke='#7a9cc6' stroke-width='0.5'/>", path_d(l, false));
        }
        let c = sh.bbox().center();
        let _ = write!(
            s,
            "<text x='{:.1}' y='{:.1}' font-family='sans-serif' font-size='{:.0}' text-anchor='middle' dominant-baseline='middle' fill='#333'>{}</text>",
            c.x * k,
            h - c.y * k,
            (sh.bbox().height().min(sh.bbox().width()) * k * 0.25).clamp(8.0, 28.0),
            part.name
        );
    }
    if let Some(x) = limit {
        let _ = write!(s, "<line x1='{0:.1}' y1='0' x2='{0:.1}' y2='{h:.1}' stroke='#d22' stroke-width='3' stroke-dasharray='12 6'/>", x * k);
    }
    s.push_str("</svg>");
    std::fs::write(path, s).with_context(|| format!("{}: cannot write SVG", path.display()))
}
