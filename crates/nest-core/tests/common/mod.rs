//! Генератор синтетических DXF для тестов.
#![allow(dead_code)]

use dxf::entities::{Arc, Circle, Ellipse, Entity, EntityType, Line, LwPolyline, Spline};
use dxf::enums::{AcadVersion, Units};
use dxf::{Drawing, LwPolylineVertex, Point, Vector};
use std::f64::consts::PI;
use std::path::Path;

pub fn on(layer: &str, specific: EntityType) -> Entity {
    let mut e = Entity::new(specific);
    e.common.layer = layer.into();
    e
}

/// Замкнутая LWPOLYLINE; `bulges[i]` относится к сегменту i → i+1.
pub fn lwpoly(pts: &[(f64, f64)], bulges: &[f64], closed: bool) -> EntityType {
    let mut pl = LwPolyline::default();
    pl.set_is_closed(closed);
    pl.vertices = pts
        .iter()
        .enumerate()
        .map(|(i, &(x, y))| LwPolylineVertex { x, y, bulge: bulges.get(i).copied().unwrap_or(0.0), ..Default::default() })
        .collect();
    EntityType::LwPolyline(pl)
}

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> EntityType {
    lwpoly(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], &[], true)
}

pub fn line(a: (f64, f64), b: (f64, f64)) -> EntityType {
    EntityType::Line(Line::new(Point::new(a.0, a.1, 0.0), Point::new(b.0, b.1, 0.0)))
}

/// Дуга против часовой стрелки, углы в градусах.
pub fn arc(c: (f64, f64), r: f64, a0: f64, a1: f64) -> EntityType {
    EntityType::Arc(Arc::new(Point::new(c.0, c.1, 0.0), r, a0, a1))
}

pub fn circle(c: (f64, f64), r: f64) -> EntityType {
    EntityType::Circle(Circle::new(Point::new(c.0, c.1, 0.0), r))
}

pub fn ellipse(c: (f64, f64), major: (f64, f64), ratio: f64) -> EntityType {
    EntityType::Ellipse(Ellipse {
        center: Point::new(c.0, c.1, 0.0),
        major_axis: Vector::new(major.0, major.1, 0.0),
        minor_axis_ratio: ratio,
        start_parameter: 0.0,
        end_parameter: 2.0 * PI,
        ..Default::default()
    })
}

/// Зажатый кубический B-сплайн по контрольным точкам.
pub fn spline(ctrl: &[(f64, f64)]) -> EntityType {
    let n = ctrl.len();
    let p = 3;
    let inner = n - p;
    let mut knots = vec![0.0; p + 1];
    knots.extend((1..inner).map(|i| i as f64 / inner as f64));
    knots.extend(vec![1.0; p + 1]);
    EntityType::Spline(Spline {
        degree_of_curve: p as i32,
        knot_values: knots,
        control_points: ctrl.iter().map(|&(x, y)| Point::new(x, y, 0.0)).collect(),
        ..Default::default()
    })
}

pub fn save(dir: &Path, name: &str, units: Units, entities: Vec<Entity>) {
    let mut d = Drawing::new();
    d.header.version = AcadVersion::R2000;
    d.header.default_drawing_units = units;
    for e in entities {
        d.add_entity(e);
    }
    d.save_file(dir.join(format!("{name}.dxf"))).unwrap();
}

/// Набор деталей для тестов. Возвращает (имя, ожидаемая площадь, мм²).
pub fn write_fixtures(dir: &Path) -> Vec<(&'static str, f64)> {
    let mm = Units::Millimeters;
    // Прямоугольник со смещением от начала координат.
    save(dir, "rect", mm, vec![on("CUT", rect(1000.0, 2000.0, 300.0, 200.0))]);

    // L-образная деталь из отдельных LINE в перемешанном порядке и направлении.
    let l = [(0.0, 0.0), (300.0, 0.0), (300.0, 100.0), (100.0, 100.0), (100.0, 300.0), (0.0, 300.0)];
    let mut lines: Vec<Entity> = (0..6).map(|i| on("CUT", line(l[i], l[(i + 1) % 6]))).collect();
    lines.swap(0, 3);
    lines.swap(1, 5);
    if let EntityType::Line(ln) = &mut lines[2].specific {
        std::mem::swap(&mut ln.p1, &mut ln.p2);
    }
    save(dir, "lshape", mm, lines);

    // Рамка с двумя отверстиями: круг и прямоугольник.
    save(
        dir,
        "frame",
        mm,
        vec![
            on("CUT", rect(0.0, 0.0, 400.0, 300.0)),
            on("CUT", circle((120.0, 150.0), 60.0)),
            on("CUT", rect(250.0, 100.0, 100.0, 50.0)),
        ],
    );

    // «Стадион» из LINE + ARC.
    save(
        dir,
        "stadium",
        mm,
        vec![
            on("CUT", line((100.0, 0.0), (400.0, 0.0))),
            on("CUT", arc((400.0, 100.0), 100.0, 270.0, 90.0)),
            on("CUT", line((400.0, 200.0), (100.0, 200.0))),
            on("CUT", arc((100.0, 100.0), 100.0, 90.0, 270.0)),
        ],
    );

    // Прямоугольник со скруглёнными углами через bulge (tan(90°/4)).
    let b = (PI / 8.0).tan();
    save(
        dir,
        "rounded",
        mm,
        vec![on(
            "CUT",
            lwpoly(
                &[(20.0, 0.0), (180.0, 0.0), (200.0, 20.0), (200.0, 130.0), (180.0, 150.0), (20.0, 150.0), (0.0, 130.0), (0.0, 20.0)],
                &[0.0, b, 0.0, b, 0.0, b, 0.0, b],
                true,
            ),
        )],
    );

    // Эллипс.
    save(dir, "ellipse", mm, vec![on("CUT", ellipse((0.0, 0.0), (150.0, 0.0), 0.5))]);

    // Деталь «D»: прямая + сплайн.
    save(
        dir,
        "dee",
        mm,
        vec![
            on("CUT", line((0.0, 300.0), (0.0, 0.0))),
            on("CUT", spline(&[(0.0, 0.0), (200.0, 0.0), (200.0, 300.0), (0.0, 300.0)])),
        ],
    );

    // Прямоугольник 10 × 5 дюймов.
    save(dir, "inch", Units::Inches, vec![on("0", rect(0.0, 0.0, 10.0, 5.0))]);

    let b_area = 200.0 * 150.0 - 4.0 * 20.0 * 20.0 + PI * 20.0 * 20.0;
    // Сплайн с 4 точками = кубическая кривая Безье: x(t) = 600t(1−t), y(t) = 300(3t² − 2t³),
    // площадь ∫x·y′dt = 1 080 000·∫t²(1−t)²dt = 36 000.
    let dee_area = 36_000.0;
    vec![
        ("rect", 60_000.0),
        ("lshape", 300.0 * 100.0 + 100.0 * 200.0),
        ("frame", 120_000.0 - PI * 3600.0 - 5000.0),
        ("stadium", 300.0 * 200.0 + PI * 100.0 * 100.0),
        ("rounded", b_area),
        ("ellipse", PI * 150.0 * 75.0),
        ("dee", dee_area),
        ("inch", 254.0 * 127.0),
    ]
}
