//! Independent check of a layout on the original geometry (crate geo), without any solver code.

use crate::geom::{BBox, Demand, Ring, Sheet};
use geo::{Area, BooleanOps, Distance, Euclidean, LineString, Polygon};
use std::fmt;

/// Allowance for the solver's f32 rounding, mm.
pub const EPS: f64 = 0.01;

#[derive(Debug, Clone)]
pub enum Violation {
    /// A part sticks out of the sheet by `by` mm.
    OutOfSheet { item: usize, by: f64 },
    /// Two parts overlap by `area` mm².
    Overlap { a: usize, b: usize, area: f64 },
    /// Two parts are closer than the gap.
    Gap { a: usize, b: usize, dist: f64 },
}

#[derive(Debug, Clone)]
pub struct Validation {
    /// Placement labels: "name #copy".
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
            return write!(f, "validation passed: {} parts, {} pairs checked", self.labels.len(), self.pairs_checked);
        }
        writeln!(f, "validation FAILED, violations: {}", self.violations.len())?;
        for v in &self.violations {
            match *v {
                Violation::OutOfSheet { item, by } => {
                    writeln!(f, "  {} sticks out of the sheet by {by:.3} mm", self.labels[item])?
                }
                Violation::Overlap { a, b, area } => {
                    writeln!(f, "  {} and {} overlap ({area:.3} mm²)", self.labels[a], self.labels[b])?
                }
                Violation::Gap { a, b, dist } => writeln!(
                    f,
                    "  {} and {}: distance {dist:.3} mm is below the gap",
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

/// Checks that parts are inside the sheet, do not overlap and are at least `gap` apart.
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
        // Nesting uses the outer contour; nothing may be placed inside holes.
        polys.push(Polygon::new(ring_to_ls(&s.outer), vec![]));
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
            part: Part { name: "sq".into(), source: "sq.dxf".into(), shape: Shape::new(outer, vec![]), ..Default::default() },
            qty: 2,
            filler: false,
        }
    }

    fn at(x: f64, y: f64) -> Placement {
        Placement { part: 0, transform: Transform { rotation_deg: 0.0, translation: Pt::new(x, y) } }
    }

    #[test]
    fn detects_overlap_gap_and_bounds() {
        let d = [square(10.0)];
        let sheet = |ps| Sheet { width: 100.0, length: 100.0, used_length: 100.0, placements: ps };
        assert!(validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(12.0, 0.0)]), 2.0).ok());
        let v = validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(11.0, 0.0)]), 2.0);
        assert!(matches!(v.violations[..], [Violation::Gap { .. }]));
        let v = validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(5.0, 5.0)]), 0.0);
        assert!(matches!(v.violations[..], [Violation::Overlap { .. }]));
        let v = validate_sheet(&d, &sheet(vec![at(95.0, 0.0)]), 0.0);
        assert!(matches!(v.violations[..], [Violation::OutOfSheet { .. }]));
        // Touching is fine with a zero gap.
        assert!(validate_sheet(&d, &sheet(vec![at(0.0, 0.0), at(10.0, 0.0)]), 0.0).ok());
    }
}
