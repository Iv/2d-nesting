//! Writes a sheet to DXF: layer SHEET (sheet rectangle), PARTS (part geometry), LABELS (names).

use crate::geom::{point_in_ring, Demand, Pt, Ring, Shape, Sheet};
use anyhow::{Context, Result};
use dxf::entities::{Entity, EntityType, LwPolyline, Text};
use dxf::LwPolylineVertex;
use dxf::enums::{AcadVersion, HorizontalTextJustification, Units, VerticalTextJustification};
use dxf::tables::Layer;
use dxf::{Color, Drawing, Point};
use geo::InteriorPoint;
use std::path::Path;

pub const LAYER_SHEET: &str = "SHEET";
pub const LAYER_PARTS: &str = "PARTS";
pub const LAYER_LABELS: &str = "LABELS";

pub fn write_sheet(path: &Path, demands: &[Demand], sheet: &Sheet) -> Result<()> {
    let mut d = Drawing::new();
    d.header.version = AcadVersion::R2000;
    d.header.default_drawing_units = Units::Millimeters;
    for (name, color) in [(LAYER_SHEET, 8u8), (LAYER_PARTS, 7), (LAYER_LABELS, 3)] {
        d.add_layer(Layer { name: name.into(), color: Color::from_index(color), ..Default::default() });
    }

    let rect = vec![
        Pt::new(0.0, 0.0),
        Pt::new(sheet.length, 0.0),
        Pt::new(sheet.length, sheet.width),
        Pt::new(0.0, sheet.width),
    ];
    d.add_entity(polyline(&rect, LAYER_SHEET));

    for p in &sheet.placements {
        let part = &demands[p.part].part;
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
