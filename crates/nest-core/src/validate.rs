//! Независимая проверка раскладки на исходной геометрии (crate geo), без кода солвера.

use crate::geom::{BBox, Demand, Ring, Sheet};
use geo::{Area, BooleanOps, Distance, Euclidean, LineString, Polygon};
use std::fmt;

/// Допуск на погрешность f32 в солвере, мм.
pub const EPS: f64 = 0.01;

#[derive(Debug, Clone)]
pub enum Violation {
    /// Деталь выходит за пределы листа на `by` мм.
    OutOfSheet { item: usize, by: f64 },
    /// Детали перекрываются на площади `area` мм².
    Overlap { a: usize, b: usize, area: f64 },
    /// Расстояние между деталями меньше зазора.
    Gap { a: usize, b: usize, dist: f64 },
}

#[derive(Debug, Clone)]
pub struct Validation {
    /// Подписи размещений: "имя #номер_копии".
    pub labels: Vec<String>,
    pub violations: Vec<Violation>,
    pub pairs_checked: usize,
}

impl Validation {
    pub fn ok(&self) -> bool {
        self.violations.is_empty()
    }
}

impl fmt::Display for Validation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ok() {
            return write!(f, "проверка пройдена: {} деталей, {} пар проверено", self.labels.len(), self.pairs_checked);
        }
        writeln!(f, "проверка НЕ пройдена, нарушений: {}", self.violations.len())?;
        for v in &self.violations {
            match *v {
                Violation::OutOfSheet { item, by } => {
                    writeln!(f, "  {} выходит за лист на {by:.3} мм", self.labels[item])?
                }
                Violation::Overlap { a, b, area } => {
                    writeln!(f, "  {} и {} перекрываются ({area:.3} мм²)", self.labels[a], self.labels[b])?
                }
                Violation::Gap { a, b, dist } => writeln!(
                    f,
                    "  {} и {}: расстояние {dist:.3} мм меньше зазора",
                    self.labels[a], self.labels[b]
                )?,
            }
        }
        Ok(())
    }
}

fn ring_to_ls(r: &Ring) -> LineString<f64> {
    let mut pts: Vec<(f64, f64)> = r.iter().map(|p| (p.x, p.y)).collect();
    pts.push(pts[0]);
    LineString::from(pts)
}

/// Проверяет: детали внутри листа, не пересекаются, расстояние между ними не меньше `gap`.
pub fn validate_sheet(demands: &[Demand], sheet: &Sheet, gap: f64) -> Validation {
    let mut copy_no = vec![0usize; demands.len()];
    let mut labels = Vec::new();
    let mut polys = Vec::new();
    let mut boxes = Vec::new();
    for p in &sheet.placements {
        copy_no[p.part] += 1;
        labels.push(format!("{} #{}", demands[p.part].part.name, copy_no[p.part]));
        let s = demands[p.part].part.shape.transformed(&p.transform);
        boxes.push(s.bbox());
        polys.push(Polygon::new(ring_to_ls(&s.outer), s.holes.iter().map(ring_to_ls).collect()));
    }

    let mut violations = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        let by = [-b.min.x, -b.min.y, b.max.x - sheet.length, b.max.y - sheet.width]
            .into_iter()
            .fold(0.0, f64::max);
        if by > EPS {
            violations.push(Violation::OutOfSheet { item: i, by });
        }
    }

    let near = |a: &BBox, b: &BBox| {
        a.min.x <= b.max.x + gap && b.min.x <= a.max.x + gap && a.min.y <= b.max.y + gap && b.min.y <= a.max.y + gap
    };
    let mut pairs_checked = 0;
    for i in 0..polys.len() {
        for j in i + 1..polys.len() {
            if !near(&boxes[i], &boxes[j]) {
                continue;
            }
            pairs_checked += 1;
            let area = polys[i].intersection(&polys[j]).unsigned_area();
            if area > EPS * EPS {
                violations.push(Violation::Overlap { a: i, b: j, area });
                continue;
            }
            if gap > 0.0 {
                let dist = Euclidean.distance(&polys[i], &polys[j]);
                if dist < gap - EPS {
                    violations.push(Violation::Gap { a: i, b: j, dist });
                }
            }
        }
    }
    Validation { labels, violations, pairs_checked }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Part, Placement, Pt, Shape, Transform};

    fn square(size: f64) -> Demand {
        let outer = vec![Pt::new(0.0, 0.0), Pt::new(size, 0.0), Pt::new(size, size), Pt::new(0.0, size)];
        Demand {
            part: Part { name: "sq".into(), source: "sq.dxf".into(), shape: Shape::new(outer, vec![]) },
            qty: 2,
        }
    }

    fn at(x: f64, y: f64) -> Placement {
        Placement { part: 0, transform: Transform { rotation_deg: 0.0, translation: Pt::new(x, y) } }
    }

    #[test]
    fn detects_overlap_gap_and_bounds() {
        let d = [square(10.0)];
        let sheet = |ps| Sheet { width: 100.0, length: 100.0, placements: ps };
        assert!(validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(12.0, 0.0)]), 2.0).ok());
        let v = validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(11.0, 0.0)]), 2.0);
        assert!(matches!(v.violations[..], [Violation::Gap { .. }]));
        let v = validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(5.0, 5.0)]), 0.0);
        assert!(matches!(v.violations[..], [Violation::Overlap { .. }]));
        let v = validate_sheet(&d, &sheet(vec![at(95.0, 0.0)]), 0.0);
        assert!(matches!(v.violations[..], [Violation::OutOfSheet { .. }]));
        // Касание при нулевом зазоре допустимо.
        assert!(validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(10.0, 0.0)]), 0.0).ok());
    }
}
