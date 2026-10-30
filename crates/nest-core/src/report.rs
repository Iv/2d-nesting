//! Plain-text layout report.

use crate::geom::{Demand, NestResult};
use crate::solver::{Budget, NestOutcome, NestParams};
use crate::validate::Validation;
use std::fmt::Write;

pub fn render(demands: &[Demand], params: &NestParams, outcome: &NestOutcome, validations: &[Validation]) -> String {
    let r: &NestResult = &outcome.result;
    let mut s = String::new();
    let _ = writeln!(s, "Nesting report");
    let _ = writeln!(s, "==============");
    let _ = writeln!(s, "Fabric width:        {:.0} mm", params.width);
    let _ = writeln!(s, "Gap between parts:   {} mm", params.gap);
    let angles: Vec<String> = params.rotations.iter().map(|a| format!("{a}°")).collect();
    let tol = if params.rotation_tolerance > 0.0 {
        format!(" ± {}° (step {}°)", params.rotation_tolerance, params.rotation_step)
    } else {
        String::new()
    };
    let _ = writeln!(s, "Allowed angles:      {}{tol}", angles.join(", "));
    let budget = match params.budget {
        Budget::Time(d) => format!("time {} s", d.as_secs_f32()),
        Budget::Iterations(n) => format!("{n} iterations (reproducible)"),
    };
    let _ = writeln!(s, "Search budget:       {budget}, seed {}, workers {}", outcome.seed, params.workers);
    let _ = writeln!(s, "Run time:            {:.1} s", outcome.elapsed.as_secs_f32());
    let _ = writeln!(s);

    let part_area = |i: usize| demands[i].part.shape.area();
    for (n, sheet) in r.sheets.iter().enumerate() {
        let placed_area: f64 = sheet.placements.iter().map(|p| part_area(p.part)).sum();
        let sheet_area = sheet.width * sheet.length;
        let util = if sheet_area > 0.0 { placed_area / sheet_area * 100.0 } else { 0.0 };
        let lower = placed_area / sheet.width;
        let _ = writeln!(s, "Sheet {}", n + 1);
        let _ = writeln!(s, "  Used length:        {:.1} mm", sheet.length);
        let _ = writeln!(s, "  Fabric used:        {:.3} m", sheet.length / 1000.0);
        let _ = writeln!(s, "  Parts area:         {:.4} m² (by outer contour)", placed_area / 1e6);
        let _ = writeln!(s, "  Sheet area:         {:.4} m²", sheet_area / 1e6);
        let _ = writeln!(s, "  Utilisation:        {util:.2}%");
        let _ = writeln!(
            s,
            "  Length lower bound: {lower:.1} mm (area / width; layout is {:.1}% longer)",
            if lower > 0.0 { (sheet.length / lower - 1.0) * 100.0 } else { 0.0 }
        );
        let _ = writeln!(s, "  Parts on sheet:     {}", sheet.placements.len());
        if sheet.length > params.max_length {
            let _ = writeln!(
                s,
                "  WARNING: length {:.1} mm exceeds the maximum {:.0} mm",
                sheet.length, params.max_length
            );
        }
        if let Some(v) = validations.get(n) {
            let _ = writeln!(s, "  {}", v.to_string().trim_end().replace('\n', "\n  "));
        }
        let _ = writeln!(s);
    }

    let placed: usize = r.sheets.iter().map(|sh| sh.placements.len()).sum();
    let unplaced: usize = r.unplaced.iter().map(|u| u.qty).sum();
    let _ = writeln!(s, "Placed parts:   {placed}");
    let _ = writeln!(s, "Unplaced parts: {unplaced}");
    for u in &r.unplaced {
        let _ = writeln!(s, "  {} × {}: {}", demands[u.part].part.name, u.qty, u.reason);
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "Parts:");
    let w = demands.iter().map(|d| d.part.name.chars().count()).max().unwrap_or(4).max(6);
    let _ = writeln!(s, "  {:<w$}  {:>6}  {:>12}  {:>12}  {:>6}", "part", "qty", "area, mm²", "net, mm²", "inner");
    for d in demands {
        let _ = writeln!(
            s,
            "  {:<w$}  {:>6}  {:>12.1}  {:>12.1}  {:>6}",
            d.part.name,
            d.qty,
            d.part.shape.area(),
            d.part.shape.net_area(),
            d.part.shape.holes.len() + d.part.shape.lines.len()
        );
    }
    s
}
