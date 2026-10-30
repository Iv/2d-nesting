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

    // Нижняя граница по площади и разумный верх.
    let area: f64 = sheet.placements.iter().map(|pl| job.demands[pl.part].part.shape.area()).sum();
    assert!(sheet.length >= area / sheet.width);
    assert!(sheet.length < 2.0 * area / sheet.width + 500.0, "длина {}", sheet.length);

    // Детали только 0° или 180°.
    assert!(sheet.placements.iter().all(|pl| [0.0, 180.0].contains(&pl.transform.rotation_deg)));

    // DXF читается обратно: 1 прямоугольник листа, контуры с отверстиями (frame × 2 = по 3 контура), подписи.
    let (s, parts, labels) = layer_counts(&job.sheet_files[0]);
    assert_eq!((s, parts, labels), (1, 23 + 2 * 2, 23));
    let report = fs::read_to_string(out.path().join("report.txt")).unwrap();
    assert!(report.contains("Использование ткани") && report.contains("проверка пройдена"));
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
    // Деталь шире ткани при любом повороте → неразмещённая.
    save(input.path(), "huge", dxf::enums::Units::Millimeters, vec![on("0", rect(0.0, 0.0, 1700.0, 1700.0))]);
    let out = tempfile::tempdir().unwrap();
    let job = nest_core::run(input.path(), out.path(), &ImportOptions::default(), &params(2.0, vec![0.0], 1)).unwrap();
    assert!(job.valid(), "{}", job.validations[0]);
    let r = &job.outcome.result;
    assert!(r.sheets[0].placements.iter().all(|pl| pl.transform.rotation_deg == 0.0));
    assert_eq!(r.unplaced.len(), 1);
    assert_eq!(job.demands[r.unplaced[0].part].part.name, "huge");
    assert!(job.report.contains("Не размещено деталей: 1"));
}
