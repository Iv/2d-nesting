//! Текстовый отчёт о раскладке.

use crate::geom::{Demand, NestResult};
use crate::solver::{Budget, NestOutcome, NestParams};
use crate::validate::Validation;
use std::fmt::Write;

pub fn render(demands: &[Demand], params: &NestParams, outcome: &NestOutcome, validations: &[Validation]) -> String {
    let r: &NestResult = &outcome.result;
    let mut s = String::new();
    let _ = writeln!(s, "Отчёт о раскладке");
    let _ = writeln!(s, "=================");
    let _ = writeln!(s, "Ширина ткани:        {:.0} мм", params.width);
    let _ = writeln!(s, "Зазор между деталями: {} мм", params.gap);
    let angles: Vec<String> = params.rotations.iter().map(|a| format!("{a}°")).collect();
    let _ = writeln!(s, "Разрешённые углы:    {}", angles.join(", "));
    let budget = match params.budget {
        Budget::Time(d) => format!("время {} с", d.as_secs_f32()),
        Budget::Iterations(n) => format!("{n} итераций (воспроизводимо)"),
    };
    let _ = writeln!(s, "Остановка поиска:    {budget}, seed {}, потоков {}", outcome.seed, params.workers);
    let _ = writeln!(s, "Время расчёта:       {:.1} с", outcome.elapsed.as_secs_f32());
    let _ = writeln!(s);

    let part_area = |i: usize| demands[i].part.shape.area();
    for (n, sheet) in r.sheets.iter().enumerate() {
        let placed_area: f64 = sheet.placements.iter().map(|p| part_area(p.part)).sum();
        let sheet_area = sheet.width * sheet.length;
        let util = if sheet_area > 0.0 { placed_area / sheet_area * 100.0 } else { 0.0 };
        let lower = placed_area / sheet.width;
        let _ = writeln!(s, "Отрез {}", n + 1);
        let _ = writeln!(s, "  Использованная длина: {:.1} мм", sheet.length);
        let _ = writeln!(s, "  Расход ткани:         {:.3} м", sheet.length / 1000.0);
        let _ = writeln!(s, "  Площадь деталей:      {:.4} м²", placed_area / 1e6);
        let _ = writeln!(s, "  Площадь отреза:       {:.4} м²", sheet_area / 1e6);
        let _ = writeln!(s, "  Использование ткани:  {util:.2}%");
        let _ = writeln!(
            s,
            "  Нижняя граница длины: {lower:.1} мм (площадь / ширина; раскладка длиннее на {:.1}%)",
            if lower > 0.0 { (sheet.length / lower - 1.0) * 100.0 } else { 0.0 }
        );
        let _ = writeln!(s, "  Деталей на отрезе:    {}", sheet.placements.len());
        if sheet.length > params.max_length {
            let _ = writeln!(
                s,
                "  ВНИМАНИЕ: длина {:.1} мм больше максимальной {:.0} мм",
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
    let _ = writeln!(s, "Размещено деталей:    {placed}");
    let _ = writeln!(s, "Не размещено деталей: {unplaced}");
    for u in &r.unplaced {
        let _ = writeln!(s, "  {} × {}: {}", demands[u.part].part.name, u.qty, u.reason);
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "Детали:");
    let w = demands.iter().map(|d| d.part.name.chars().count()).max().unwrap_or(4).max(6);
    let _ = writeln!(s, "  {:<w$}  {:>5}  {:>12}  {:>6}", "деталь", "кол-во", "площадь, мм²", "отв.");
    for d in demands {
        let _ = writeln!(
            s,
            "  {:<w$}  {:>5}  {:>12.1}  {:>6}",
            d.part.name,
            d.qty,
            d.part.shape.area(),
            d.part.shape.holes.len()
        );
    }
    s
}
