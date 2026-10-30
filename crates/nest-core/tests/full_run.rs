mod common;

use common::*;
use dxf::entities::EntityType;
use dxf::Drawing;
use nest_core::dxf_in::ImportOptions;
use nest_core::solver::{Budget, NestParams};
use std::fs;
use std::path::Path;

const ITERATIONS: usize = 150;

fn job_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    fs::write(
        dir.path().join("parts.toml"),
        "[parts]\nrect = 4\nlshape = 4\nframe = 2\nstadium = 3\nrounded = 3\nellipse = 2\ndee = 3\ninch = { qty = 2 }\n",
    )
    .unwrap();
    dir
}

fn params(gap: f64, rotations: Vec<f64>, seed: u64) -> NestParams {
    NestParams {
        gap,
        rotations,
        budget: Budget::Iterations(ITERATIONS),
        seed: Some(seed),
        workers: 2,
        ..Default::default()
    }
}

fn layer_counts(path: &Path) -> (usize, usize, usize) {
    let d = Drawing::load_file(path).unwrap();
    let count = |l: &str| d.entities().filter(|e| e.common.layer == l).count();
    let texts = d.entities().filter(|e| matches!(e.specific, EntityType::Text(_))).count();
    assert_eq!(texts, count("LABELS"));
    (count("SHEET"), count("PARTS"), count("LABELS"))
}

#[test]
fn full_run_with_gap_is_valid() {
    let input = job_dir();
    let out = tempfile::tempdir().unwrap();
    let p = params(3.0, vec![0.0, 180.0], 42);
    let job = nest_core::run(input.path(), out.path(), &ImportOptions::default(), &p).unwrap();

    assert!(job.valid(), "{}", job.validations[0]);
    let sheet = &job.outcome.result.sheets[0];
    assert_eq!(sheet.placements.len(), 23);
    assert!(job.outcome.result.unplaced.is_empty());

    // Area lower bound and a sane upper bound.
    let area: f64 = sheet.placements.iter().map(|pl| job.demands[pl.part].part.shape.area()).sum();
    assert!(sheet.used_length >= area / sheet.width);
    assert!(sheet.used_length < 2.0 * area / sheet.width + 500.0, "length {}", sheet.used_length);

    // Only 0° or 180°.
    assert!(sheet.placements.iter().all(|pl| [0.0, 180.0].contains(&pl.transform.rotation_deg)));

    // The DXF reads back: 1 sheet rectangle, contours with holes (frame × 2 = 3 contours each), labels.
    let (s, parts, labels) = layer_counts(&job.sheet_files[0]);
    assert_eq!((s, parts, labels), (1, 23 + 2 * 2, 23));
    let report = fs::read_to_string(out.path().join("report.txt")).unwrap();
    assert!(report.contains("Utilisation") && report.contains("validation passed"));
}

#[test]
fn same_seed_and_iterations_give_same_layout() {
    let input = job_dir();
    let run = |seed| {
        let out = tempfile::tempdir().unwrap();
        let job = nest_core::run(input.path(), out.path(), &ImportOptions::default(), &params(0.0, vec![0.0, 180.0], seed)).unwrap();
        assert!(job.valid());
        job.outcome.result.sheets[0]
            .placements
            .iter()
            .map(|pl| (pl.part, pl.transform.rotation_deg.to_bits(), pl.transform.translation.x.to_bits(), pl.transform.translation.y.to_bits()))
            .collect::<Vec<_>>()
    };
    assert_eq!(run(7), run(7));
}

#[test]
fn fixed_orientation_and_too_wide_parts() {
    let input = job_dir();
    // A part wider than the fabric at every angle ends up unplaced.
    save(input.path(), "huge", dxf::enums::Units::Millimeters, vec![on("0", rect(0.0, 0.0, 1700.0, 1700.0))]);
    let out = tempfile::tempdir().unwrap();
    let job = nest_core::run(input.path(), out.path(), &ImportOptions::default(), &params(2.0, vec![0.0], 1)).unwrap();
    assert!(job.valid(), "{}", job.validations[0]);
    let r = &job.outcome.result;
    assert!(r.sheets[0].placements.iter().all(|pl| pl.transform.rotation_deg == 0.0));
    assert_eq!(r.unplaced.len(), 1);
    assert_eq!(job.demands[r.unplaced[0].part].part.name, "huge");
    assert!(job.report.contains("Unplaced parts: 1"));
}

#[test]
fn rotation_tolerance_expands_angles() {
    let p = NestParams { rotations: vec![0.0, 90.0], rotation_tolerance: 2.0, rotation_step: 1.0, ..Default::default() };
    assert_eq!(p.allowed_angles(), vec![0.0, 1.0, 2.0, 88.0, 89.0, 90.0, 91.0, 92.0, 358.0, 359.0]);

    let input = job_dir();
    let out = tempfile::tempdir().unwrap();
    let p = NestParams { rotation_tolerance: 10.0, rotation_step: 5.0, ..params(1.0, vec![0.0, 90.0, 180.0, 270.0], 3) };
    let job = nest_core::run(input.path(), out.path(), &ImportOptions::default(), &p).unwrap();
    assert!(job.valid(), "{}", job.validations[0]);
    let allowed = p.allowed_angles();
    assert!(job.outcome.result.sheets[0].placements.iter().all(|pl| allowed.contains(&pl.transform.rotation_deg)));
}

#[test]
fn parts_are_spread_over_fixed_size_sheets() {
    let input = job_dir();
    let out = tempfile::tempdir().unwrap();
    // A short sheet: the 23 parts need several sheets.
    let p = NestParams { width: 700.0, length: 900.0, ..params(2.0, vec![0.0, 90.0, 180.0, 270.0], 5) };
    let job = nest_core::run(input.path(), out.path(), &ImportOptions::default(), &p).unwrap();
    let r = &job.outcome.result;
    assert!(r.sheets.len() >= 3, "{} sheets", r.sheets.len());
    assert!(r.unplaced.is_empty());
    assert_eq!(r.sheets.iter().map(|s| s.placements.len()).sum::<usize>(), 23);
    for (s, v) in r.sheets.iter().zip(&job.validations) {
        assert!(v.ok(), "{v}");
        assert_eq!((s.width, s.length), (700.0, 900.0));
        assert!(s.used_length <= 900.0 + 1e-6);
    }
    // Every part copy is placed exactly as often as demanded.
    let mut count = vec![0; job.demands.len()];
    for s in &r.sheets {
        for pl in &s.placements {
            count[pl.part] += 1;
        }
    }
    assert!(job.demands.iter().zip(&count).all(|(d, &c)| d.qty == c));
    assert_eq!(job.sheet_files.len(), r.sheets.len());
    assert!(out.path().join("index.html").is_file());
}

/// Panels (trapezoids named top*/bot*) with left copies, and a rib that is free to tilt.
fn panel_job() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mm = dxf::enums::Units::Millimeters;
    let trapezoid = |l: f64, h: f64, a: f64, b: f64| lwpoly(&[(0.0, 0.0), (l, 0.0), (l - a, h), (b, h)], &[], true);
    save(dir.path(), "top1", mm, vec![on("CUT", trapezoid(1400.0, 250.0, 120.0, 40.0))]);
    save(dir.path(), "bot1", mm, vec![on("CUT", trapezoid(1100.0, 200.0, 30.0, 150.0))]);
    save(dir.path(), "rib1", mm, vec![on("CUT", trapezoid(900.0, 120.0, 400.0, 0.0))]);
    fs::write(dir.path().join("parts.toml"), "[parts]\ntop1 = { qty = 2, left = 2 }\nbot1 = { left = 2 }\nrib1 = 2\n").unwrap();
    dir
}

#[test]
fn left_copies_are_mirrored() {
    let input = panel_job();
    let demands = nest_core::manifest::load_job(input.path(), &ImportOptions::default()).unwrap();
    let names: Vec<(&str, usize)> = demands.iter().map(|d| (d.part.name.as_str(), d.qty)).collect();
    assert_eq!(names, vec![("bot1_L", 2), ("rib1", 2), ("top1", 2), ("top1_L", 2)]);
    let (r, l) = (&demands[2].part.shape, &demands[3].part.shape);
    assert!((r.area() - l.area()).abs() < 1e-6 && l.area() > 0.0);
    assert_eq!((l.bbox().min.x, l.bbox().max.x), (-r.bbox().max.x, -r.bbox().min.x));
    // Not a rotation of the original: the slanted edges change sides.
    assert!(l.outer.iter().any(|p| (p.x, p.y) == (-1280.0, 250.0)));
}

#[test]
fn panels_are_kept_straight() {
    let p = NestParams { tilt_penalty: 10.0, rotation_tolerance: 10.0, rotation_step: 2.0, ..params(3.0, vec![0.0, 90.0, 180.0, 270.0], 7) };
    assert!(p.is_panel("Top1_L") && p.is_panel("bot3") && !p.is_panel("rib1"));
    assert_eq!(p.tilt(352.0), 8.0);
    assert_eq!(p.tilt_cost("top1", 92.0), 40.0);
    assert_eq!(p.tilt_cost("rib1", 92.0), 0.0);

    let input = panel_job();
    let out = tempfile::tempdir().unwrap();
    let job = nest_core::run(input.path(), out.path(), &ImportOptions::default(), &p).unwrap();
    assert!(job.valid());
    let sheets = &job.outcome.result.sheets;
    let panels: Vec<f64> = sheets
        .iter()
        .flat_map(|s| &s.placements)
        .filter(|pl| p.is_panel(&job.demands[pl.part].part.name))
        .map(|pl| p.tilt(pl.transform.rotation_deg))
        .collect();
    assert_eq!(panels.len(), 6);
    assert!(panels.iter().all(|&d| d < 1e-3), "panel tilts {panels:?}");
    assert!(job.report.contains("Panels tilted:      0 of 6"));
}

#[test]
fn tilted_panels_are_aligned_to_their_long_axis() {
    use nest_core::geom::{Demand, Part, Pt, Shape, Transform};
    let strip = Shape::new(vec![Pt::new(0.0, 0.0), Pt::new(1200.0, 0.0), Pt::new(1150.0, 300.0), Pt::new(30.0, 300.0)], vec![]);
    let drawn = strip.transformed(&Transform { rotation_deg: 7.0, translation: Pt::new(50.0, 80.0) });
    let part = |name: &str, shape: &Shape| Demand { part: Part { name: name.into(), source: "x.dxf".into(), shape: shape.clone() }, qty: 1, filler: false };
    // A chunky panel and a rib keep their drawn angle.
    let square = Shape::new(vec![Pt::new(0.0, 0.0), Pt::new(300.0, 40.0), Pt::new(260.0, 340.0), Pt::new(-40.0, 300.0)], vec![]);
    let mut demands = vec![part("top1", &drawn), part("top2", &square), part("rib1", &drawn), part("bot1", &strip)];
    let (axis, elongation) = drawn.long_axis();
    assert!((axis - 7.0).abs() < 0.05 && elongation > 3.0, "{axis} {elongation}");

    let turned = nest_core::solver::align_panels(&mut demands, &NestParams::default());
    assert_eq!(turned.len(), 1);
    assert_eq!(turned[0].0, 0);
    assert!((turned[0].1 + 7.0).abs() < 0.05);
    assert!(demands[0].part.shape.long_axis().0.abs() < 0.05);
    assert_eq!(demands[1].part.shape, square);
    assert_eq!(demands[2].part.shape, drawn);
}

#[test]
fn fillers_go_into_free_space_only() {
    let input = job_dir();
    let fill = tempfile::tempdir().unwrap();
    let mm = dxf::enums::Units::Millimeters;
    save(fill.path(), "chip", mm, vec![on("CUT", rect(0.0, 0.0, 60.0, 40.0))]);
    save(fill.path(), "huge", mm, vec![on("CUT", rect(0.0, 0.0, 5000.0, 5000.0))]);
    fs::write(fill.path().join("parts.toml"), "[parts]\nchip = 500\nhuge = 1\n").unwrap();
    let p = NestParams { width: 700.0, length: 900.0, ..params(2.0, vec![0.0, 90.0, 180.0, 270.0], 5) };

    let (o1, o2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let plain = nest_core::run(input.path(), o1.path(), &ImportOptions::default(), &p).unwrap();
    let filled = nest_core::run_with_fillers(input.path(), &[fill.path().into()], o2.path(), &ImportOptions::default(), &p).unwrap();
    assert!(filled.valid());
    let (a, b) = (&plain.outcome.result, &filled.outcome.result);
    // The same sheets (fillers come after them), none longer, and no new sheet.
    assert_eq!(a.sheets.len(), b.sheets.len());
    for (x, y) in a.sheets.iter().zip(&b.sheets) {
        assert!(y.used_length <= x.used_length + 1e-6, "{} > {}", y.used_length, x.used_length);
    }
    let chip = filled.demands.iter().position(|d| d.part.name == "chip").unwrap();
    assert!(filled.demands[chip].filler);
    let n = b.sheets.iter().flat_map(|s| &s.placements).filter(|pl| pl.part == chip).count();
    assert!(n > 0 && n < 500, "{n} chips");
    // A filler that fits nowhere is not reported as unplaced.
    assert!(b.unplaced.is_empty());
    assert!(filled.report.contains(&format!("Fillers placed: {n} of 501 offered")));
}

#[test]
fn free_strip_on_top_is_not_counted() {
    use nest_core::geom::{Demand, Part, Placement, Pt, Shape, Transform};
    let shape = Shape::new(vec![Pt::new(0.0, 0.0), Pt::new(1000.0, 0.0), Pt::new(1000.0, 1000.0), Pt::new(0.0, 1000.0)], vec![]);
    let demands = vec![Demand { part: Part { name: "sq".into(), source: "sq.dxf".into(), shape }, qty: 1, filler: false }];
    let at = |y: f64| vec![Placement { part: 0, transform: Transform { rotation_deg: 0.0, translation: Pt::new(0.0, y) } }];
    let p = NestParams { width: 1560.0, min_strip: 150.0, strip_ratio: 1.0, ..Default::default() };
    // 560 mm free on top: 1000 mm of length consume 1000 · 1000/1560.
    assert_eq!(p.top_strip(&demands, &at(0.0)), 560.0);
    assert!((p.consumed(&demands, &at(0.0), 1000.0) - 1000.0 * 1000.0 / 1560.0).abs() < 1e-9);
    // Worth half as much as fabric saved at the end: only 280 mm of the strip count as saved.
    let half = NestParams { strip_ratio: 2.0, ..p.clone() };
    assert!((half.consumed(&demands, &at(0.0), 1000.0) - 1000.0 * (1560.0 - 280.0) / 1560.0).abs() < 1e-9);
    // 100 mm is too narrow to reuse.
    assert_eq!(p.top_strip(&demands, &at(460.0)), 0.0);
    assert_eq!(p.consumed(&demands, &at(460.0), 1000.0), 1000.0);
    assert_eq!(NestParams { min_strip: 0.0, ..p }.top_strip(&demands, &at(0.0)), 0.0);
}
