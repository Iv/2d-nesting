//! Запись отреза в DXF: слой SHEET (прямоугольник отреза), PARTS (контуры деталей), LABELS (имена).

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
        d.add_entity(label(&part.name, &s));
    }

    d.save_file(path).with_context(|| format!("{}: не удалось записать DXF", path.display()))
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
    // Высота текста: так, чтобы имя примерно помещалось в деталь.
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
