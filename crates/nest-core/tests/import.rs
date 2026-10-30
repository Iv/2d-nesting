mod common;

use common::*;
use dxf::enums::Units;
use nest_core::dxf_in::{read_part, ImportOptions};

#[test]
fn fixtures_import_with_expected_areas() {
    let dir = tempfile::tempdir().unwrap();
    let opts = ImportOptions::default();
    for (name, area) in write_fixtures(dir.path()) {
        let part = read_part(&dir.path().join(format!("{name}.dxf")), &opts).unwrap();
        let got = part.shape.net_area();
        // Хорды лежат внутри дуг, поэтому площадь кривых немного меньше точной.
        let rel = (got - area).abs() / area;
        assert!(rel < 0.005, "{name}: площадь {got:.1}, ожидалось {area:.1}");
        assert_eq!(part.name, name);
    }
}

#[test]
fn holes_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    let part = read_part(&dir.path().join("frame.dxf"), &ImportOptions::default()).unwrap();
    assert_eq!(part.shape.holes.len(), 2);
}

#[test]
fn inches_are_converted() {
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    let b = read_part(&dir.path().join("inch.dxf"), &ImportOptions::default()).unwrap().shape.bbox();
    assert!((b.width() - 254.0).abs() < 1e-9 && (b.height() - 127.0).abs() < 1e-9);
}

#[test]
fn chord_error_respects_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), "c", Units::Millimeters, vec![on("0", circle((0.0, 0.0), 500.0))]);
    for tol in [1.0, 0.2, 0.05] {
        let opts = ImportOptions { tolerance: tol, ..Default::default() };
        let ring = read_part(&dir.path().join("c.dxf"), &opts).unwrap().shape.outer;
        for (i, p) in ring.iter().enumerate() {
            let q = ring[(i + 1) % ring.len()];
            let mid = ((p.x + q.x) / 2.0).hypot((p.y + q.y) / 2.0);
            assert!(500.0 - mid <= tol + 1e-9, "tol {tol}: отклонение {}", 500.0 - mid);
        }
    }
}

#[test]
fn open_contour_is_reported_with_file_and_layer() {
    let dir = tempfile::tempdir().unwrap();
    save(
        dir.path(),
        "broken",
        Units::Millimeters,
        vec![
            on("CUT", line((0.0, 0.0), (100.0, 0.0))),
            on("CUT", line((100.0, 0.0), (100.0, 100.0))),
            on("CUT", line((100.0, 100.0), (0.0, 100.0))),
            on("GRAIN", line((50.0, 10.0), (50.0, 90.0))),
        ],
    );
    let strict = ImportOptions { allow_open_inside: false, ..Default::default() };
    let err = format!("{:#}", read_part(&dir.path().join("broken.dxf"), &strict).unwrap_err());
    assert!(err.contains("broken.dxf"), "{err}");
    assert!(err.contains("незамкнут"), "{err}");
    assert!(err.contains("CUT") && err.contains("GRAIN"), "{err}");

    // После исключения слоя GRAIN остаётся только незамкнутый CUT.
    let opts = ImportOptions { ignore_layers: vec!["grain".into()], allow_open_inside: false, ..Default::default() };
    let err = format!("{:#}", read_part(&dir.path().join("broken.dxf"), &opts).unwrap_err());
    assert!(!err.contains("GRAIN"), "{err}");
}

#[test]
fn layer_filter_selects_cut_line() {
    let dir = tempfile::tempdir().unwrap();
    save(
        dir.path(),
        "two",
        Units::Millimeters,
        vec![on("CUT", rect(0.0, 0.0, 100.0, 100.0)), on("SEAM", rect(10.0, 10.0, 80.0, 80.0))],
    );
    let all = read_part(&dir.path().join("two.dxf"), &ImportOptions::default()).unwrap();
    assert_eq!(all.shape.holes.len(), 1);
    let opts = ImportOptions { layers: Some(vec!["cut".into()]), ..Default::default() };
    let cut = read_part(&dir.path().join("two.dxf"), &opts).unwrap();
    assert!(cut.shape.holes.is_empty());
}

#[test]
fn second_part_in_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    save(
        dir.path(),
        "pair",
        Units::Millimeters,
        vec![on("0", rect(0.0, 0.0, 100.0, 100.0)), on("0", rect(200.0, 0.0, 50.0, 50.0))],
    );
    let err = format!("{:#}", read_part(&dir.path().join("pair.dxf"), &ImportOptions::default()).unwrap_err());
    assert!(err.contains("не лежит внутри"), "{err}");
}

#[test]
fn open_lines_inside_are_kept_and_nested_contours_allowed() {
    let dir = tempfile::tempdir().unwrap();
    save(
        dir.path(),
        "rib",
        Units::Millimeters,
        vec![
            on("0", rect(0.0, 0.0, 300.0, 100.0)),
            on("0", rect(20.0, 20.0, 100.0, 60.0)),
            on("0", rect(40.0, 40.0, 20.0, 20.0)), // контур внутри выреза
            on("0", line((150.0, 10.0), (150.0, 90.0))),
        ],
    );
    let s = read_part(&dir.path().join("rib.dxf"), &ImportOptions::default()).unwrap().shape;
    assert_eq!((s.holes.len(), s.lines.len()), (2, 1));
    assert!((s.area() - 30_000.0).abs() < 1e-6);
    assert!((s.net_area() - (30_000.0 - 6000.0 + 400.0)).abs() < 1e-6);

    // Разорванный внешний контур не маскируется под «линию внутри».
    save(
        dir.path(),
        "torn",
        Units::Millimeters,
        vec![
            on("0", line((0.0, 0.0), (300.0, 0.0))),
            on("0", line((300.0, 0.0), (300.0, 100.0))),
            on("0", line((300.0, 100.0), (0.0, 100.0))),
            on("0", rect(20.0, 20.0, 50.0, 50.0)),
        ],
    );
    let err = format!("{:#}", read_part(&dir.path().join("torn.dxf"), &ImportOptions::default()).unwrap_err());
    assert!(err.contains("выходят за"), "{err}");
}
