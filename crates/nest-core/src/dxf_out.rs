//! Writes a sheet to DXF. By default every placed part is an INSERT of a block holding its source
//! drawing unchanged (its own layers, labels and marks); the sheet rectangle is on layer SHEET.
//! With `outlines` the parts are written as the traced contours on layer PARTS with a large name
//! label on layer LABELS, which is easier to check by eye.

use crate::geom::{point_in_ring, Demand, Part, Pt, Ring, Shape, Sheet};
use anyhow::{Context, Result};
use dxf::entities::{Entity, EntityType, Insert, LwPolyline, Text};
use dxf::LwPolylineVertex;
use dxf::enums::{AcadVersion, HorizontalTextJustification, Units, VerticalTextJustification};
use dxf::tables::Layer;
use dxf::{Block, Color, Drawing, Point};
use geo::InteriorPoint;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const LAYER_SHEET: &str = "SHEET";
pub const LAYER_PARTS: &str = "PARTS";
pub const LAYER_LABELS: &str = "LABELS";

/// How sheets are written.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputOptions {
    /// Traced contours and large name labels instead of the source drawings as blocks.
    pub outlines: bool,
}

pub fn write_sheet(path: &Path, demands: &[Demand], sheet: &Sheet, opts: &OutputOptions) -> Result<()> {
    let mut d = Drawing::new();
    d.header.version = AcadVersion::R2000;
    d.header.default_drawing_units = Units::Millimeters;
    let layers: &[(&str, u8)] =
        if opts.outlines { &[(LAYER_SHEET, 8), (LAYER_PARTS, 7), (LAYER_LABELS, 3)] } else { &[(LAYER_SHEET, 8)] };
    for &(name, color) in layers {
        d.add_layer(Layer { name: name.into(), color: Color::from_index(color), ..Default::default() });
    }

    let rect = vec![
        Pt::new(0.0, 0.0),
        Pt::new(sheet.length, 0.0),
        Pt::new(sheet.length, sheet.width),
        Pt::new(0.0, sheet.width),
    ];
    d.add_entity(polyline(&rect, LAYER_SHEET));

    let mut blocks = Blocks::default();
    for p in &sheet.placements {
        let part = &demands[p.part].part;
        if !opts.outlines {
            let name = blocks.block_for(&mut d, &part.source)?;
            d.add_entity(insert(name, part, &p.transform));
            continue;
        }
        let s = part.shape.transformed(&p.transform);
        d.add_entity(polyline(&s.outer, LAYER_PARTS));
        for h in &s.holes {
            d.add_entity(polyline(h, LAYER_PARTS));
        }
        for l in &s.lines {
            d.add_entity(open_polyline(l, LAYER_PARTS));
        }
        d.add_entity(label(&part.name, &s));
    }

    d.save_file(path).with_context(|| format!("{}: cannot write DXF", path.display()))
}

/// Source drawings copied into the sheet as blocks, one per source file (left copies use the
/// right-hand block mirrored).
#[derive(Default)]
struct Blocks {
    by_source: HashMap<PathBuf, String>,
}

impl Blocks {
    fn block_for(&mut self, d: &mut Drawing, source: &Path) -> Result<String> {
        if let Some(n) = self.by_source.get(source) {
            return Ok(n.clone());
        }
        let src = Drawing::load_file(source).with_context(|| format!("{}: cannot read DXF", source.display()))?;
        let stem = source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut name = block_name(&stem);
        while d.blocks().any(|b| b.name.eq_ignore_ascii_case(&name)) {
            name.push('_');
        }
        // The source's own layers, line types and text styles, unless the sheet has one of that name.
        for l in src.layers() {
            if !d.layers().any(|o| o.name.eq_ignore_ascii_case(&l.name)) {
                d.add_layer(l.clone());
            }
        }
        for lt in src.line_types() {
            if !d.line_types().any(|o| o.name.eq_ignore_ascii_case(&lt.name)) {
                d.add_line_type(lt.clone());
            }
        }
        for st in src.styles() {
            if !d.styles().any(|o| o.name.eq_ignore_ascii_case(&st.name)) {
                d.add_style(st.clone());
            }
        }
        // Blocks the part uses (directly or nested; files cut from an assembly carry all of its
        // blocks), renamed under the part's block so different files cannot clash.
        let by_name: HashMap<String, &Block> = src.blocks().map(|b| (b.name.to_ascii_uppercase(), b)).collect();
        let mut used: Vec<&Block> = Vec::new();
        let mut todo: Vec<String> = src.entities().filter_map(referenced_block).collect();
        while let Some(n) = todo.pop() {
            if let Some(&b) = by_name.get(&n.to_ascii_uppercase())
                && !is_layout_block(&b.name)
                && !used.iter().any(|u| std::ptr::eq(*u, b))
            {
                used.push(b);
                todo.extend(b.entities.iter().filter_map(referenced_block));
            }
        }
        let inner: HashMap<String, String> = used
            .iter()
            .map(|b| (b.name.to_ascii_uppercase(), block_name(&format!("{name}${}", b.name.trim_start_matches('*')))))
            .collect();
        let rename = |e: &Entity| {
            let mut e = e.clone();
            match &mut e.specific {
                EntityType::Insert(ins) => {
                    if let Some(n) = inner.get(&ins.name.to_ascii_uppercase()) {
                        ins.name = n.clone();
                    }
                }
                EntityType::RotatedDimension(dim) => rename_dim(&mut dim.dimension_base.block_name, &inner),
                EntityType::RadialDimension(dim) => rename_dim(&mut dim.dimension_base.block_name, &inner),
                EntityType::DiameterDimension(dim) => rename_dim(&mut dim.dimension_base.block_name, &inner),
                EntityType::AngularThreePointDimension(dim) => rename_dim(&mut dim.dimension_base.block_name, &inner),
                EntityType::OrdinateDimension(dim) => rename_dim(&mut dim.dimension_base.block_name, &inner),
                _ => {}
            }
            e
        };
        for b in used {
            d.add_block(Block {
                name: inner[&b.name.to_ascii_uppercase()].clone(),
                base_point: b.base_point.clone(),
                layer: b.layer.clone(),
                entities: b.entities.iter().map(rename).collect(),
                ..Default::default()
            });
        }
        d.add_block(Block {
            name: name.clone(),
            layer: "0".into(),
            entities: src.entities().map(rename).collect(),
            ..Default::default()
        });
        self.by_source.insert(source.to_path_buf(), name.clone());
        Ok(name)
    }
}

/// Name of the block an INSERT or a dimension draws.
fn referenced_block(e: &Entity) -> Option<String> {
    Some(match &e.specific {
        EntityType::Insert(i) => i.name.clone(),
        EntityType::RotatedDimension(d) => d.dimension_base.block_name.clone(),
        EntityType::RadialDimension(d) => d.dimension_base.block_name.clone(),
        EntityType::DiameterDimension(d) => d.dimension_base.block_name.clone(),
        EntityType::AngularThreePointDimension(d) => d.dimension_base.block_name.clone(),
        EntityType::OrdinateDimension(d) => d.dimension_base.block_name.clone(),
        _ => return None,
    })
}

fn rename_dim(block: &mut String, inner: &HashMap<String, String>) {
    if let Some(n) = inner.get(&block.to_ascii_uppercase()) {
        *block = n.clone();
    }
}

fn is_layout_block(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("*MODEL_SPACE") || n.starts_with("*PAPER_SPACE")
}

/// A valid DXF block name: characters DXF forbids in table names become '_'.
fn block_name(s: &str) -> String {
    s.chars().map(|c| if "<>/\\\":;?*|=`,".contains(c) || c.is_control() { '_' } else { c }).collect()
}

/// INSERT placing the source drawing where the solver put the part: the block is scaled to mm,
/// mirrored for a left copy, then turned by the alignment and placement rotations.
fn insert(block: String, part: &Part, t: &crate::geom::Transform) -> Entity {
    let k = part.drawn.scale;
    let ins = Insert {
        name: block,
        location: Point::new(t.translation.x, t.translation.y, 0.0),
        x_scale_factor: if part.drawn.mirrored { -k } else { k },
        y_scale_factor: k,
        z_scale_factor: k,
        rotation: (part.drawn.rotation_deg + t.rotation_deg).rem_euclid(360.0),
        ..Default::default()
    };
    let mut e = Entity::new(EntityType::Insert(ins));
    e.common.layer = "0".into();
    e
}

fn polyline(ring: &Ring, layer: &str) -> Entity {
    let mut pl = LwPolyline::default();
    pl.set_is_closed(true);
    pl.vertices = ring.iter().map(|p| LwPolylineVertex { x: p.x, y: p.y, ..Default::default() }).collect();
    let mut e = Entity::new(EntityType::LwPolyline(pl));
    e.common.layer = layer.into();
    e
}

fn open_polyline(pts: &[Pt], layer: &str) -> Entity {
    let mut pl = LwPolyline::default();
    pl.vertices = pts.iter().map(|p| LwPolylineVertex { x: p.x, y: p.y, ..Default::default() }).collect();
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
