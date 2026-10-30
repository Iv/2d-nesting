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
    let _ = writeln!(s, "Sheet size:          {:.0} x {:.0} mm", params.width, params.length);
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
    // A free strip along the top edge at least this wide is kept for other parts, not wasted.
    const REUSABLE_STRIP: f64 = 200.0;
    let top_strip = |sh: &crate::geom::Sheet| {
        let used_width = sh
            .placements
            .iter()
            .map(|p| demands[p.part].part.shape.transformed(&p.transform).bbox().max.y)
            .fold(0.0, f64::max);
        let free = sh.width - used_width;
        if free >= REUSABLE_STRIP { free } else { 0.0 }
    };
    let mut total_area = 0.0;
    let mut reusable_area = 0.0;
    for (n, sheet) in r.sheets.iter().enumerate() {
        let placed_area: f64 = sheet.placements.iter().map(|p| part_area(p.part)).sum();
        total_area += placed_area;
        let sheet_area = sheet.width * sheet.length;
        let used_area = sheet.width * sheet.used_length;
        let pct = |a: f64| if a > 0.0 { placed_area / a * 100.0 } else { 0.0 };
        let _ = writeln!(s, "Sheet {}", n + 1);
        let _ = writeln!(s, "  Parts on sheet:     {}", sheet.placements.len());
        let _ = writeln!(s, "  Parts area:         {:.4} m² (by outer contour)", placed_area / 1e6);
        let _ = writeln!(s, "  Used length:        {:.1} of {:.0} mm", sheet.used_length, sheet.length);
        let _ = writeln!(s, "  Utilisation:        {:.2}% of the sheet, {:.2}% of the used length", pct(sheet_area), pct(used_area));
        let strip = top_strip(sheet);
        if strip > 0.0 {
            reusable_area += strip * sheet.used_length;
            let _ = writeln!(
                s,
                "  Free top strip:     {strip:.0} mm wide (reusable); {:.2}% without it",
                pct(used_area - strip * sheet.used_length)
            );
        }
        if let Some(v) = validations.get(n) {
            let _ = writeln!(s, "  {}", v.to_string().trim_end().replace('\n', "\n  "));
        }
        let _ = writeln!(s);
    }
    if !r.sheets.is_empty() {
        // Fabric past a straight cut after the last part of a sheet is kept, not wasted.
        let used: f64 = r.sheets.iter().map(|sh| sh.used_length).sum();
        let _ = writeln!(s, "Total");
        let _ = writeln!(s, "  Sheets:             {}", r.sheets.len());
        let _ = writeln!(s, "  Fabric used:        {:.3} m (sum of the used lengths, each sheet cut straight)", used / 1000.0);
        let _ = writeln!(s, "  Parts area:         {:.4} m²", total_area / 1e6);
        let _ = writeln!(s, "  Utilisation:        {:.2}% of the used fabric", total_area / (used * params.width) * 100.0);
        if reusable_area > 0.0 {
            let _ = writeln!(
                s,
                "  Without top strips: {:.2}% (free strips of {REUSABLE_STRIP:.0} mm and more along the top are reusable)",
                total_area / (used * params.width - reusable_area) * 100.0
            );
        }
        let _ = writeln!(
            s,
            "  Lower bound:        {:.3} m of fabric, {:.2} sheets (by area)",
            total_area / params.width / 1000.0,
            total_area / (params.width * params.length)
        );
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
